---
id: elements
families: [flux, flux_schnell, flux2_klein_4b, flux2_klein_9b, flux3_klein, flux3_klein_base, z_image, krea2, qwen_image, qwen_image_21, iris, moss_image]
modes: [chat, plan, critique]
priority: 55
---
How to write a prompt for this model: elements first, then the scene.

Whenever a picture has more than one subject (people, animals) or a place that matters, the prompt is paragraphs:
1. One paragraph per subject, never two in one. It starts with who it is: their name, or "The boy is…", "The dog is…" when they have none.
2. One paragraph for the place: what is in it, the light, what is in the background.
3. Last, the scene: who is where, what each is doing, their faces, the camera. Only the names here; never describe them again.

What a subject's paragraph says, in a few concrete sentences: age, build, skin, face, eyes, hair (colour, length, how it is worn), clothes with colours and materials. A creature: its colour, what its skin or fur is like, and its size against someone in the picture ("as tall as a house"). Make every detail fit the scene: a soldier in a WW1 trench is muddy and unshaven, a bride's hair is done. The person's own words always win; add only what they left unsaid.

Paragraphs are separated by a blank line, which in the JSON string is \n\n. When you change a prompt, rewrite only the paragraph that is wrong and copy the others word for word. List the elements in `notes` first.

Example prompt:
"Larry is a 34-year-old construction worker with a scruffy, sunburnt face and brown eyes. Thick, messy black hair under a scuffed yellow hard hat; dusty blue overalls over a white shirt.\n\nLars is a thin man with a mousy face, thin lips and square glasses with thin frames. Neat, thinning brown hair; white shirt, blue tie, grey linen trousers.\n\nThe office is a site office with open blinds; the building site shows through the window. Blueprints hang on the wall behind.\n\nLars shouts at Larry, pointing at the door, furious. Larry looks down, embarrassed."
