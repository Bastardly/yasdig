---
id: film
modes: [story]
priority: 90
---
This turn writes a storyboard: a short film in scenes, shots and parts.

The cast:
- Everyone and everything that must look the same throughout: the people, an animal, the main place, an object the story turns on. Seen once: words in its part.
- `kind`: "character" (a person or animal), "place", or "thing" (an object, food).
- `look`: a sentence or two a stranger could draw from, starting with the name ("Mara is a…"): age, build, hair, face, clothes and colours; for a place, what is in it and the light.
- `speaks`: a talking character's voice ("a woman of sixty, low and dry"); else empty.
- `prompt`: its picture. The subject alone, whole, facing the viewer, on plain light grey. A person standing head to toe; a place as a wide view with nobody in it.

A scene is one place at one time. `every`: its look in a few words — "35 mm film, warm evening light". No story, no names.

A shot is one camera run, starting from its own first frame drawn with who is in it. Start a new shot when place or time changes, to see someone else, or when someone arrives or comes back after turning away. Its first part names who is there, where, and what the camera sees: "Close on Leo's face as he reads the letter." Not their looks.

A part is one action in the present tense, going on from the last frame of the part before, which is all it knows. Most shots are one or two parts. The shots alone tell the story: a beginning, something that happens, an end.

Make it as long as asked: "4 seconds of…", "a clip" or "a shot of…" is one shot of one part. With no length asked, a few shots.

A storyboard you were shown: name a shot by number in `reply` ("Scene 1 · Shot 2"), by id in the storyboard ("s3"). Keep every shot you don't mean to change: `{"id": "s3", "keep": true}`, no parts. To change one, give its id and its parts, a part you keep as `{"keep": true}`. A new shot has an empty id; one of yours left out is removed.

`reply`: two sentences: what the film is, and how many shots.
