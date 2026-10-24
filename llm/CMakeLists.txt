# yasdig-llm: the agent's one-turn driver (yasdig-llm.cpp), built against the
# gitignored llama.cpp clone beside this folder. build.rs configures this
# project, not llama.cpp's, so only the driver and what it links are compiled.
cmake_minimum_required(VERSION 3.16)
project(yasdig-llm CXX C)

set(LLAMA_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../llama.cpp" CACHE PATH "llama.cpp checkout")

# llama.cpp as a library: common (chat templates, sampling, the JSON-schema
# grammar) and libmtmd (vision) without the server, the tools or a TLS stack.
set(LLAMA_BUILD_COMMON   ON  CACHE BOOL "" FORCE)
set(LLAMA_BUILD_MTMD     ON  CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TOOLS    OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_SERVER   OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_TESTS    OFF CACHE BOOL "" FORCE)
set(LLAMA_BUILD_APP      OFF CACHE BOOL "" FORCE)
set(LLAMA_OPENSSL        OFF CACHE BOOL "" FORCE)
set(LLAMA_SUBPROCESS     OFF CACHE BOOL "" FORCE)
set(MTMD_VIDEO           OFF CACHE BOOL "" FORCE)
set(BUILD_SHARED_LIBS    OFF CACHE BOOL "" FORCE)

add_subdirectory("${LLAMA_DIR}" llama.cpp EXCLUDE_FROM_ALL)

add_executable(yasdig-llm yasdig-llm.cpp)
target_link_libraries(yasdig-llm PRIVATE llama-common mtmd vendor::nlohmann)
target_compile_features(yasdig-llm PRIVATE cxx_std_17)
set_target_properties(yasdig-llm PROPERTIES RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")
