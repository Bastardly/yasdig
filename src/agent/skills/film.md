---
id: film
modes: [story]
priority: 90
---
This turn writes a storyboard: a short film in scenes, shots and parts.

The cast:
- Everyone and everything that must look the same throughout: the people, an animal, the main place, an object the story turns on. A thing seen once is words in its part.
- `look`: one or two sentences a stranger could draw from, starting with the name ("Mara is a…"): age, build, hair, face, clothes and colours; for a place, what is in it and the light.
- `prompt`: its picture. The subject alone, whole, facing the viewer, on plain light grey, in the film's look. A person standing head to toe; a place as a wide view with nobody in it.
- `video_model`, when asked: H3 when people speak or sound matters, else Wan, unless the person named one.

A scene is one place at one time. `every`: its look in a few words — "35 mm film, warm evening light". No story, no names. A locked scene keeps its look.

A shot is one camera run, starting from its own first frame drawn with who is in it. Start a new shot when place or time changes, to see someone else, or when someone arrives or comes back after turning away. Its first part names who is there, where, and what the camera sees: "Close on Leo's face as he reads the letter." Not their looks.

A part is one action in the present tense, going on from the last frame of the part before, which is all it knows. Most shots are one or two parts. The shots alone tell the story: a beginning, something that happens, an end.

A storyboard you were shown: each shot has an id ("s3"). Keep every shot you don't mean to change: `{"id": "s3", "keep": true}`, no parts. To change one, give its id and all its parts. A new shot has an empty id. A shot left out is removed, unless it is generated.

`reply`: in two sentences, what the film is and how many shots; its pictures are drawn when the person presses Draw the cast.
