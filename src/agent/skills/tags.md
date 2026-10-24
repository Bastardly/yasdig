---
id: tags
families: [sd1, sd2, sdxl]
priority: 50
---
This model reads tags better than sentences: short comma-separated phrases, most important first, e.g. "portrait of a fox, red knitted hat, snowy forest, soft morning light, shallow depth of field, photograph".
- (word:1.2) makes a phrase count more, (word:0.8) less. Stay between 0.8 and 1.4.
- The negative prompt matters here: "blurry, lowres, extra fingers, watermark, text" is a fair start.
- CFG 5–8 is usual; above 10 colours burn.
