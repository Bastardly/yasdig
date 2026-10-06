# Feature - Help page, sampler and scheduler

Currently, we only have a few samplers and no scheduler settings.

It would be great if we could select those, and begin the work on a help page on various topics.
I'm thinking that the Help tab could be placed before settings, and be a simple page with a side menu and a content field. Same widths as we use on the generation page

I believe that stable-diffusion.cpp supports all of these.

And a help page with a short explanation on when to use what. Imagine that you are explaining very concisely what each sampler and scheduler does, and when to use it. Not wordy, just enough to give the user an idea of what to use.

## Samplers 

### For samplers I was thinking something like 
euler	Deterministic	Simple, predictable baseline
heun, dpm2	Deterministic	Higher-order, more accurate but about 2x slower per step
dpm++2m, dpm++2mv2	Deterministic	Fast and converges well; popular for SDXL-style models
ipndm, ipndm_v	Deterministic	Multistep, fast convergence at low step counts
ddim_trailing	Deterministic	DDIM variant, often used with distilled or few-step models
euler_a	Stochastic	Re-injects noise each step, more variation
dpm++2s_a	Stochastic	Ancestral version of DPM++ 2S
lcm, tcd	Few-step	Meant for LCM/TCD-distilled models; both add noise during sampling





## Scheduler
### For the samplers I was thinking something like:
discrete -	The model's native noise levels, spaced evenly in the way it was trained. The default and the safest choice.

karras -	From Karras et al. 2022. It packs more steps into the low-noise end, which often gives cleaner fine detail. Very popular with DPM++ samplers.
exponential	- Evenly spaced on a log scale, which is similar in spirit to Karras but with a different distribution.
sgm_uniform	- Uniform spacing that ends exactly at zero noise. Commonly used with distilled and few-step models.
simple - 	A straightforward even pick from the model's noise levels.
ays	- "Align Your Steps", precomputed schedules tuned to give good results in very few steps.
lcm -	For LCM-distilled models. The CLI sets it automatically if you pick the lcm sampler.
