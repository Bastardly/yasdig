# Feature 1 - Preparation for more models

This is an open-ended definition where I try to explain future plans. The goal here is to prepare for that future by making the architecture more modular. Any suggestions are simply suggestions, and any ideas based on deeper insight would be appreciated. 

Currently the app is build up on two types of models: SDXL based image models and Wan video model. However, the image model structure is a bit of a mess if we want to add more models of either type, or expand with other model families such as:
- LTX for video
- SD1 - for image
- flux, flex, krea etc. for image 
- zImage for image

This concern regards how we structure our model files. Currently, all SDXL models are in the root of the models folder. 

One suggestion could be:

/models
/models/image/SDXL
/models/image/zImage
/models/upscalers
/models/video/wan
/models/video/ltx

Another aspect is the UI.
The image tab is wired to SDXL models. But some models, like zImage, does not take negative prompts. So part of the UI would not make sense.
So one suggestion could be, to add a select box where the user can choose the model type, e.g. SDXL family, and then a definition file handles what fields are available. Same idea could be applied to video on the video tab. 

Another aspect that could be improved is the settings. Right now, we have general settings that applies to all models. It would be cool to be able to have individual settings for each model type - however, this can become rather granular defining all model types. E.g. SD1, SD2, SDXL, SDXL-turbo. So perhaps this would make more sense to do per model? We already have a preset that mimics that, but is not included in the UI.

One more issue could be the image-to-image tab. Perhaps we could take a leaf from the video tab, and use an image loader from e.g. outputs, whether it is image-to-image or text-to-image. Heck, if we do this correctly, perhaps we could even have one generate tab to handle both image and video - but this is just my ideas going haywire. But it would be cool.

Lastly, I do plan on having an image-edit tab, I believe there are both flux and zImage models that can handle edits. Meaning, an AI that can take an image, I could then mark areas in the image I wish to change, add a prompt and then hopefully the model can fix whatever it is. But that's a far away feature yet, but worth keeping in mind.

So, again. Anything is up for discussion.
