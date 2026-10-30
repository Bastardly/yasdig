// yasdig-llm: one chat turn of a llama.cpp vision-language model, then exit.
//
// yasdig's agent runs a model the way it runs sd-cli: one subprocess per turn,
// inside the same sandbox, with nothing kept between turns. llama.cpp's own tools
// don't fit that shape (checked at v0.7.0): `llama-mtmd-cli` is one-shot only with
// both `-p` and `--image` and can't be handed a conversation, and `llama-cli`
// talks to an in-process llama-server over a loopback TCP port, which the sandbox
// refuses. So this is mtmd-cli's evaluation loop (tools/mtmd/mtmd-cli.cpp) with the
// history read from a request file instead of a terminal — a file, because the
// Windows sandbox launcher hands the child a NUL stdin.
//
// The chat template, the JSON-schema grammar and the thinking switch are
// llama.cpp's (`common_chat_session`), so a model's own template decides how the
// turn is laid out.
//
// stdout carries only lines that start with a record tag (anything else on stdout
// is a library log line and is ignored by `src/llm.rs`); text is JSON-escaped so
// a record is always one line:
//
//   @yasdig-llm 1                  protocol version, first
//   P load <done> <total>          loading the weights (0..100)
//   P read <done> <total>          reading the prompt, one count per text/image chunk
//   T "<text>"                     generated text, as it comes
//   C {"content":…,"reasoning":…}  the reply, the template's own parse of it
//   D {"n_prompt":…, …}            done; stats
//   E {"kind":…, "msg":…}          failure; exit code says the same
//                                  (kind "crash" on Windows: the exception and
//                                  the module it happened in, then the OS's code)
//
// Exit codes: 0 done, 1 internal, 2 bad request, 3 load failed, 4 context too
// small, 5 out of memory (125 is the sandbox launcher's, never ours).

#include "build-info.h"
#include "chat.h"
#include "common.h"
#include "llama.h"
#include "log.h"
#include "mtmd-helper.h"
#include "mtmd.h"
#include "sampling.h"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <chrono>
#include <clocale>
#include <cstdio>
#include <cstring>
#include <fstream>
#include <string>
#include <vector>

#if defined(_WIN32)
#    ifndef WIN32_LEAN_AND_MEAN
#        define WIN32_LEAN_AND_MEAN
#    endif
#    ifndef NOMINMAX
#        define NOMINMAX
#    endif
#    include <windows.h>
#endif

using json = nlohmann::ordered_json;

namespace {

constexpr int PROTOCOL = 1;

enum Exit { OK = 0, INTERNAL = 1, BAD_REQUEST = 2, LOAD = 3, CONTEXT = 4, OOM = 5 };

void record(const char * tag, const std::string & body) {
    std::string line = std::string(tag) + " " + body + "\n";
    fwrite(line.data(), 1, line.size(), stdout);
    fflush(stdout);
}

[[noreturn]] void fail(Exit code, const char * kind, const std::string & msg, json extra = json::object()) {
    extra["kind"] = kind;
    extra["msg"]  = msg;
    record("E", extra.dump());
    fflush(stdout);
    std::exit(code);
}

int64_t now_ms() {
    return std::chrono::duration_cast<std::chrono::milliseconds>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
}

// A piece may end inside a UTF-8 sequence (byte-level BPE splits them); hold the
// tail back until it is whole, or the JSON escape would carry a broken character.
size_t utf8_complete(const std::string & s) {
    size_t n = s.size();
    for (size_t back = 1; back <= 4 && back <= n; back++) {
        unsigned char c = s[n - back];
        if ((c & 0xC0) == 0x80) {
            continue;  // continuation byte: keep looking for the lead
        }
        size_t need = (c & 0x80) == 0 ? 1 : (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 1;
        return back >= need ? n : n - back;
    }
    return n;
}

// llama.cpp's own log is written by a worker thread, so a crash loses every
// line still queued: a Windows divide-by-zero (2026-10-30) left stderr empty.
// This one writes each line before returning, so the last says how far it got.
void log_now(ggml_log_level level, const char * text, void *) {
    static ggml_log_level last = GGML_LOG_LEVEL_INFO;
    if (level != GGML_LOG_LEVEL_CONT) {
        last = level;
    }
    if (last == GGML_LOG_LEVEL_DEBUG) {
        return;
    }
    fputs(text, stderr);
    fflush(stderr);
}

#if defined(_WIN32)
const char * exception_name(DWORD code) {
    switch (code) {
        case EXCEPTION_ACCESS_VIOLATION:      return "access violation";
        case EXCEPTION_INT_DIVIDE_BY_ZERO:    return "integer divide by zero";
        case EXCEPTION_ILLEGAL_INSTRUCTION:   return "illegal instruction";
        case EXCEPTION_STACK_OVERFLOW:        return "stack overflow";
        case EXCEPTION_IN_PAGE_ERROR:         return "in-page error";
        case EXCEPTION_PRIV_INSTRUCTION:      return "privileged instruction";
        default:                              return "exception";
    }
}

// An unhandled exception becomes an `E` record naming it and the module it was
// raised in (file name only: a path would carry the user's folders), so the
// page says *where* — the driver, llama.cpp, or the Vulkan driver's DLL. The
// process then ends with the exception's own code, as it would have.
LONG WINAPI on_crash(EXCEPTION_POINTERS * e) {
    const EXCEPTION_RECORD * r = e->ExceptionRecord;
    char      path[MAX_PATH]   = "unknown module";
    uintptr_t offset           = 0;
    HMODULE   m                = nullptr;
    if (GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           (LPCSTR) r->ExceptionAddress, &m) &&
        GetModuleFileNameA(m, path, MAX_PATH)) {
        offset = (uintptr_t) r->ExceptionAddress - (uintptr_t) m;
    }
    const char * slash = strrchr(path, '\\');
    const char * file  = slash ? slash + 1 : path;
    char         msg[MAX_PATH + 128];
    snprintf(msg, sizeof msg, "crashed: %s (0x%08lX) in %s+0x%llx", exception_name(r->ExceptionCode),
             (unsigned long) r->ExceptionCode, file, (unsigned long long) offset);
    fflush(stderr);
    record("E", json{ { "kind", "crash" }, { "msg", msg } }.dump());
    return EXCEPTION_EXECUTE_HANDLER;
}
#endif

struct LoadProgress {
    int last = -1;
};

bool on_load(float progress, void * user) {
    auto * p = static_cast<LoadProgress *>(user);
    int pct  = (int) (progress * 100.0f);
    if (pct != p->last) {
        p->last = pct;
        record("P", "load " + std::to_string(pct) + " 100");
    }
    return true;
}

}  // namespace

int main(int argc, char ** argv) {
    std::setlocale(LC_NUMERIC, "C");
#if defined(_WIN32)
    SetUnhandledExceptionFilter(on_crash);
#endif

    std::string request_path;
    for (int i = 1; i < argc; i++) {
        std::string a = argv[i];
        if (a == "--version") {
            printf("yasdig-llm %d llama.cpp %s\n", PROTOCOL, llama_build_info());
            return OK;
        }
        if (a == "--request" && i + 1 < argc) {
            request_path = argv[++i];
        }
    }
    record("@yasdig-llm", std::to_string(PROTOCOL));
    if (request_path.empty()) {
        fail(BAD_REQUEST, "bad_request", "usage: yasdig-llm --request <file.json>");
    }

    json req;
    try {
        std::ifstream f(request_path, std::ios::binary);
        if (!f) {
            fail(BAD_REQUEST, "bad_request", "can't read " + request_path);
        }
        req = json::parse(f);
    } catch (const std::exception & e) {
        fail(BAD_REQUEST, "bad_request", std::string("request is not JSON: ") + e.what());
    }
    if (req.value("v", 0) != PROTOCOL) {
        fail(BAD_REQUEST, "bad_request", "request protocol " + std::to_string(req.value("v", 0)) + ", this driver speaks " +
                                             std::to_string(PROTOCOL));
    }

    common_init();
    llama_log_set(log_now, nullptr);  // after common_init, which sets its own
    ggml_backend_load_all();

    common_params params;
    params.model.path   = req.value("model", std::string());
    params.mmproj.path  = req.value("mmproj", std::string());
    params.n_ctx        = req.value("n_ctx", 8192);
    params.n_batch      = req.value("n_batch", 2048);
    params.n_ubatch     = req.value("n_ubatch", 512);
    params.n_predict    = req.value("n_predict", 768);
    params.n_gpu_layers = req.value("gpu_layers", -1);
    params.warmup       = false;  // one turn: a warmup is a second turn's worth of load
    // n_threads defaults to -1 ("pick for me"), resolved only by the argument
    // parser we don't run. Left at -1, the threadpool sizes itself from it as
    // unsigned and asks for 2^64 bytes ("insufficient memory ... 17592186044416 MB").
    postprocess_cpu_params(params.cpuparams, nullptr);
    postprocess_cpu_params(params.cpuparams_batch, &params.cpuparams);
    LoadProgress load_progress;
    params.load_progress_callback           = on_load;
    params.load_progress_callback_user_data = &load_progress;
    if (req.contains("sampling")) {
        const auto & s        = req.at("sampling");
        params.sampling.temp  = s.value("temp", params.sampling.temp);
        params.sampling.top_p = s.value("top_p", params.sampling.top_p);
        params.sampling.top_k = s.value("top_k", params.sampling.top_k);
        params.sampling.min_p = s.value("min_p", params.sampling.min_p);
        params.sampling.seed  = s.value("seed", params.sampling.seed);
        params.sampling.penalty_repeat = s.value("repeat_penalty", params.sampling.penalty_repeat);
    }
    if (params.model.path.empty()) {
        fail(BAD_REQUEST, "bad_request", "no model in the request");
    }

    const int64_t t_load = now_ms();
    auto init            = common_init_from_params(params);
    llama_model *   model = init->model();
    llama_context * lctx  = init->context();
    if (!model || !lctx) {
        fail(LOAD, "load", "llama.cpp could not load " + params.model.path);
    }
    const llama_vocab * vocab = llama_model_get_vocab(model);

    mtmd::context_ptr vision;
    if (!params.mmproj.path.empty()) {
        mtmd_helper_log_set(log_now, nullptr);
        mtmd_context_params mp = mtmd_context_params_default();
        mp.use_gpu             = true;
        mp.print_timings       = false;
        mp.n_threads           = params.cpuparams.n_threads;
        mp.flash_attn_type     = params.flash_attn_type;
        mp.warmup              = false;
        // non-causal vision towers need the whole image in one ubatch (mtmd-cli does the same)
        auto mem = mtmd_get_memory_usage(params.mmproj.path.c_str(), mp);
        if (mem.use_non_causal && mem.image_max_tokens > (int) llama_n_ubatch(lctx)) {
            mp.image_max_tokens = llama_n_ubatch(lctx);
            mp.image_min_tokens = std::min(mp.image_min_tokens, (int) llama_n_ubatch(lctx));
        }
        vision.reset(mtmd_init_from_file(params.mmproj.path.c_str(), model, mp));
        if (!vision) {
            fail(LOAD, "load", "llama.cpp could not load the vision projector " + params.mmproj.path);
        }
    }
    const int64_t load_ms = now_ms() - t_load;

    // The conversation: text parts verbatim, an image part becomes the media marker
    // the template places and mtmd_tokenize swaps for the image's tokens.
    const std::string marker = mtmd_default_marker();
    common_chat_templates_inputs inputs;
    mtmd::bitmaps bitmaps;
    mtmd_helper_init_opt init_opt = mtmd_helper_init_opt_default();
    for (const auto & m : req.value("messages", json::array())) {
        common_chat_msg msg;
        msg.role = m.value("role", std::string("user"));
        const auto & content = m.contains("content") ? m.at("content") : json();
        if (content.is_string()) {
            msg.content = content.get<std::string>();
        } else if (content.is_array()) {
            for (const auto & part : content) {
                std::string type = part.value("type", std::string());
                if (type == "text") {
                    msg.content_parts.push_back({ "text", part.value("text", std::string()) });
                } else if (type == "image") {
                    if (!vision || !mtmd_support_vision(vision.get())) {
                        fail(BAD_REQUEST, "bad_request", "an image was sent to a model without a vision projector");
                    }
                    std::string path = part.value("path", std::string());
                    auto        bmp  = mtmd_helper_bitmap_init_from_file(vision.get(), path.c_str(), false, init_opt);
                    if (!bmp.bitmap) {
                        fail(BAD_REQUEST, "bad_request", "can't read image " + path);
                    }
                    bitmaps.entries.emplace_back(bmp.bitmap);
                    msg.content_parts.push_back({ "media_marker", marker });
                }
            }
        }
        inputs.messages.push_back(std::move(msg));
    }
    if (inputs.messages.empty()) {
        fail(BAD_REQUEST, "bad_request", "no messages");
    }
    if (req.contains("json_schema") && !req.at("json_schema").is_null()) {
        inputs.json_schema = req.at("json_schema").dump();
    }
    inputs.use_jinja             = true;
    inputs.add_generation_prompt = true;
    inputs.enable_thinking       = req.value("thinking", false);
    inputs.reasoning_format      = COMMON_REASONING_FORMAT_AUTO;

    auto tmpls = common_chat_templates_init(model, "");
    common_chat_session session;
    try {
        session = common_chat_session(tmpls.get(), vocab, inputs);
    } catch (const std::exception & e) {
        fail(BAD_REQUEST, "bad_request", std::string("chat template: ") + e.what());
    }
    session.apply_sampling(params.sampling);
    common_sampler * smpl = common_sampler_init(model, params.sampling);
    if (!smpl) {
        fail(BAD_REQUEST, "bad_request", "the sampler (or the JSON schema's grammar) could not be built");
    }

    // Tokenize text and images together, then evaluate chunk by chunk so the page
    // can say how far reading has got. A model without a projector reads plain tokens.
    const int64_t     t_read = now_ms();
    const std::string prompt = session.prompt().str();
    const int32_t     n_ctx  = (int32_t) llama_n_ctx(lctx);
    llama_pos         n_past = 0;
    if (vision) {
        mtmd::input_chunks chunks(mtmd_input_chunks_init());
        mtmd_input_text    text{ prompt.c_str(), prompt.size(), /* add_special */ true, /* parse_special */ true };
        auto               ptrs = bitmaps.c_ptr();
        int32_t            res  = mtmd_tokenize(vision.get(), chunks.ptr.get(), &text, ptrs.data(), ptrs.size());
        if (res != 0) {
            fail(BAD_REQUEST, "bad_request", "tokenizing the conversation failed (" + std::to_string(res) + ")");
        }
        const size_t  n_chunks = mtmd_input_chunks_size(chunks.ptr.get());
        const int32_t need     = (int32_t) mtmd_helper_get_n_pos(chunks.ptr.get()) + params.n_predict;
        if (need > n_ctx) {
            fail(CONTEXT, "context", "the conversation does not fit the context", { { "need", need }, { "have", n_ctx } });
        }
        for (size_t i = 0; i < n_chunks; i++) {
            record("P", "read " + std::to_string(i) + " " + std::to_string(n_chunks));
            llama_pos next  = n_past;
            auto      chunk = mtmd_input_chunks_get(chunks.ptr.get(), i);
            if (mtmd_helper_eval_chunk_single(vision.get(), lctx, chunk, n_past, 0, params.n_batch, i == n_chunks - 1,
                                              &next) != 0) {
                fail(INTERNAL, "internal", "evaluating part " + std::to_string(i + 1) + " of the conversation failed");
            }
            n_past = next;
        }
        record("P", "read " + std::to_string(n_chunks) + " " + std::to_string(n_chunks));
    } else {
        llama_tokens  toks = common_tokenize(vocab, prompt, true, true);
        const int32_t need = (int32_t) toks.size() + params.n_predict;
        if (need > n_ctx) {
            fail(CONTEXT, "context", "the conversation does not fit the context", { { "need", need }, { "have", n_ctx } });
        }
        const size_t steps = (toks.size() + params.n_batch - 1) / params.n_batch;
        for (size_t i = 0, k = 0; i < toks.size(); i += params.n_batch, k++) {
            record("P", "read " + std::to_string(k) + " " + std::to_string(steps));
            size_t n = std::min(toks.size() - i, (size_t) params.n_batch);
            if (llama_decode(lctx, llama_batch_get_one(toks.data() + i, (int32_t) n))) {
                fail(INTERNAL, "internal", "decoding the prompt failed");
            }
        }
        record("P", "read " + std::to_string(steps) + " " + std::to_string(steps));
        n_past = (llama_pos) toks.size();
    }
    const int64_t read_ms = now_ms() - t_read;

    // Generate.
    const int64_t     t_gen = now_ms();
    common_batch      batch(lctx);
    common_chat_input out;
    std::string       pending;
    int               n_gen = 0;
    std::string       stop  = "length";
    for (; n_gen < params.n_predict; n_gen++) {
        llama_token tok = common_sampler_sample(smpl, lctx, -1);
        common_sampler_accept(smpl, tok, true);
        if (llama_vocab_is_eog(vocab, tok)) {
            stop = "eos";
            break;
        }
        out.append(common_token_to_piece(lctx, tok, true), tok);
        pending += common_token_to_piece(lctx, tok, false);
        size_t whole = utf8_complete(pending);
        if (whole > 0) {
            record("T", json(pending.substr(0, whole)).dump());
            pending.erase(0, whole);
        }
        batch.clear();
        batch.add(tok, n_past++, 0, true);
        if (llama_process(lctx, LLAMA_PROCESS_TYPE_DECODE, batch.get())) {
            fail(INTERNAL, "internal", "decoding a token failed");
        }
    }
    if (!pending.empty()) {
        record("T", json(pending).dump());
    }
    const int64_t gen_ms = now_ms() - t_gen;

    common_chat_msg reply;
    try {
        reply = session.finish(out);
    } catch (const std::exception & e) {
        // a reply the template's parser can't read is still the model's reply
        reply.content = out.text;
        LOG_WRN("yasdig-llm: the template's parser refused the reply: %s\n", e.what());
    }
    std::string content = reply.content;
    if (content.empty() && !reply.content_parts.empty()) {
        content = reply.render_content("");
    }
    record("C", json{ { "content", content }, { "reasoning", reply.reasoning_content } }.dump());
    record("D", json{ { "n_prompt", (int) n_past - n_gen },
                      { "n_gen", n_gen },
                      { "load_ms", load_ms },
                      { "read_ms", read_ms },
                      { "gen_ms", gen_ms },
                      { "stop", stop } }
                    .dump());
    common_sampler_free(smpl);
    return OK;
}
