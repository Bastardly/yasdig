# The Assistant

**Status: reviewed by the person, 2026-11-16.** Nothing struck. Their answers are
folded in as ✅ and kept word for word under *Your answers, as written* at the end. If
a ✅ here reads differently from that, theirs wins. What's left ❓ was read off the code
and not objected to.

✅ = the person said it (a date means it came from a fix they asked for) · ❓ = Claude's
inference

---

## What it's for

✅ **Mostly: a better prompt.** The defaults are already good settings, so a newcomer
with the right prompt gets a good picture. The Assistant rarely touches the dials, and
that's fine.

✅ **Later: sparring about the picture.** Today the person mostly dictates. The goal,
as far as the agent model can carry it, is collaboration: the Assistant suggests and
the person reacts, rather than taking orders.

## Who's in charge

- ✅ **The current prompt is the source of truth.** An edit the person made by hand
  ("the shirt is blue now") stands, until they ask otherwise.
- ✅ **A dramatic rewrite starts with suggestions.** Asked to rework the prompt
  wholesale, the Assistant comes back with a few key ideas. The person then elaborates,
  gives feedback, or presses *Just do it*.
- ✅ **The person's edits win.** A storyboard part they wrote is theirs (2026-11-14).
- ✅ **The person picks the video model.** The Assistant recommends one, with a short
  reason (2026-11-14). The person called this "Perfect!": it's the model for how to ask.
- ❓ Anything long (a session, a comparison) asks first, with Start / Draw one.
  **Q (still open):** the answer to "asks first: right, or friction?" was about prompt
  edits. Is the Start card before a session fine as it is?
- ❓ It never films; nothing on a film board is drawn unasked.
- ❓ Every change it makes can be undone.

## How it talks

- ✅ **Concise and friendly, not technical.**
- ✅ **In doubt, it asks**, with a short explanation and a recommendation, as the
  video-model card does.
- ❓ It describes pictures as they are, not as hoped, and never claims a setting the
  app doesn't have.
- ✅ It never says "I can't make video" when a video model could do it (2026-11-14).

## The person doesn't know our words

✅ "A 4 s clip", "a shot" and "4 seconds of…" all mean *make a film* (2026-11-14), and
that works now. **Accepted:** if the story asked for can't fit the length asked, the
Assistant may come up short. That's fine.

---

## The jobs

*Examples in italics are the scored `agent-check` cases that hold it today.*

### 1. Answer a question

- Answers in a few lines, and doesn't draw unasked.
- ✅ **Wanted:** where a picture would show the answer ("what does CFG do?"), it
  *offers* to draw two to compare. It doesn't just draw them.
- *Example: "answers a question without drawing".*

### 2. Draw

- Writes a prompt, draws the number asked for (one if no number is given), keeps the
  person's `{variables}` and invents none.
- ✅ The prompt rewriting is good, apart from the cast (see *Cast in a prompt*).
- *Examples: "draws when asked to draw", "counts the pictures asked for", "keeps a
  prompt variable when it rewrites the prompt".*

### 3. Keep trying until it's right (a session)

- Proposes first. Then it draws, looks, judges against checks that can be *seen*,
  changes something, and goes again.
- ✅ **When it stops short:** a summary of what it tried.
- ✅ **Wanted: steer it mid-run.** While a session runs, the person can add to it in the
  chat, rather than it being locked until it ends. Their example: a knight fighting a
  dragon looked awesome, but the knight held two shields and looked the wrong way.
  They wanted to point at those while it worked, and have the checks take them on.
  **Q:** this was written under job 5 (look at a picture). It's filed here because
  "while the assistant was working" sounds like a session. Right?
- *Examples: "proposes a session for 'keep trying'", "writes checks that can be
  answered by looking", "judges a picture by what it shows".*

### 4. Which is best? (a comparison)

- Sweeps one knob over a few values on the same seeds, then shows a table and picks
  a winner. Undo goes back to before the comparison.
- ✅ **Judging anatomy and proportions** would be valuable, but it's a tall order for
  small agent models. Expect it from the big ones only.
- *Example: "proposes a comparison for 'which is best'".*

### 5. Look at a picture (describe / critique)

- ✅ Works fairly well. Refining it isn't a priority now. The ad-hoc criteria wanted
  under job 3 are the part that matters.

### 6. Enhance a prompt box

- Rewrites the box for the model in use, as one undo step.
- Video: a part knows its neighbours. An empty place stays empty, nobody speaks unless
  the words say so, and a line asked for is spoken word for word (2026-11-14).
- ✅ **Wrong today: parts of one shot restart from the first frame.** The person wrote
  "Man goes outside", "Man looks up", "Man smokes a cigar" and enhanced them. Each
  part re-described the first frame, so he "went outside" several times instead of
  the story moving on. **Each part should go on from where the last one ended.**
  **Fixed 2026-11-23** (the person: "I think it works"): from part 3 on, the brief
  quoted the shot's enhanced part 1 as "the scene opens with… keep to what it set up",
  and some seeds went outside again. Now a part inside a shot is carried on by the
  part before alone, told that what happened there is over. *Example: "LTX: each part
  goes on from the last, none goes outside again" (three parts, 3 seeds).*
- *Examples: the seven video Enhance cases, the two two-part cases, and the two image
  cast cases.*

### 7. Make a film

- Writes the storyboard on the Video tab for the model the person picked, to the
  length asked. "Shot 2" is the storyboard's shot 2 ✅ (2026-11-14). It fills an empty
  shot the person added, and leaves their parts alone.
- ✅ **Consistency is key:** the parts of a shot go together, and across shots the
  mood, sound and colours stay the same.
- ✅ **Wrong today: it can't reimagine.** It always builds on the existing prompt,
  which sometimes ends up mismatched. In a complex scene, asking one character to do
  something else loses the rest of the scene. The person suspects the wordiness of
  the cast looks in the prompt is a big part of it.
  **Q:** "reimagining the short" — is that films only, or image prompts too? And
  where's the line with "the current prompt is the source of truth"? Is reimagining
  what the dramatic-rewrite suggestions are for?
- *Examples: the three "makes a film when asked for…" cases, "changes the shot the
  person names by its storyboard number", "fills the empty shot the person added",
  "leaves a part the person wrote alone", "gives a character who talks a voice".*

### 8. Cast

- Adds a character as words first and says so. Their picture is drawn only when asked.
- *Example: "makes a cast member, and says so before it is drawn".*

---

## Cast in a prompt

✅ **A cast member's look is written once, where they're picked, and never again in
the prompt.** "Elias holding a cat" stays about Elias holding a cat. It does not become
"Elias, a weathered cowboy in his late 40s with sun-leathered skin… holding a cat".
The prompt is for what happens in the scene and what isn't defined yet. This applies
to both Enhance and the Assistant writing a prompt. It was the most annoying problem,
and the first one taken on.

- **Image tab, done 2026-11-16, awaiting a run on the GPU.** A member picked for
  their words gets their look put in front of the prompt by yasdig. So Enhance and
  the chat are told the name and the look, and told to name them and drop whatever
  repeats the look, even where the box repeats it. **What the person wrote that differs
  from the look stays** ("Elias in a blue shirt"): that's their change, per *the current
  prompt is the source of truth*.
- ✅ **The look is context, not text to copy** (2026-11-16, after trying it). Asked
  for Oliver with an animal, with no setting given, Enhance didn't describe him but
  drew on his look for the scene: his compass became how the light hits him. "This is
  great." Oliver in a tavern holding a cat came out "really good": named, not
  described, with the moorlands from his look outside the window. **One detail where
  the scene touches it is wanted; a re-description isn't.** The check allows one look
  detail and fails at two. *Examples: "Image: a picked
  cast member is named, not described", "Image: the person's change to a picked look
  stays", "Image: a look the person wrote beside a pick is taken out".*
- **H3, done 2026-11-16, awaiting a GPU run.** A part that sends a member's picture
  as a reference already has their look written into H3's subject lines by yasdig,
  so Enhance names them and doesn't describe them. Their voice isn't added, so it
  stays in the words. *Example: "H3: a member shown by a reference is named, not
  described".*
- ✅ **H3 works fine, and it builds the story parts pretty well** (the person, 2026-11-16).
- ✅ **The A/B on LTX 2.5 distilled passed** (2026-11-16): a look paragraph followed by
  a short named action ("Mara is… Mara waits at a bus stop") drew as well as the
  enhanced prose. With two people ("Mara hands Leo an umbrella") each kept their own
  look. So on LTX 2.5 the server may add the look. LTX 2.3 (a Gemma 3 encoder) and Wan
  (umT5) are untested.
- **LTX 2.5, built 2026-11-23, awaiting a GPU run.** A part can carry its shot's cast
  looks, and yasdig puts them ahead of its prompt, a paragraph each. A part opts in:
  on the Video tab by *Add cast…*, in a film by being enhanced from now on. Its words
  then name the people. A continued part carries part 1's looks. **A part written
  before keeps its looks in its prose and gets none added**, so nothing is said twice.
  The looks count against LTX's length limit. That's also the hoped-for cure for "it
  loses the scene" under job 7. *Examples: "LTX: a member whose look yasdig adds is
  named, not described", and the pair "LTX: both parts name Mara when yasdig adds her
  look".*
- **Wan and LTX 2.3: unchanged.** The look stays in the words, because nobody has
  checked whether umT5 or Gemma 3 tie "Mara is…" to "Mara waits…".

---

## Settled

- The Image chat and the Video chat are separate, one per tab. ✅ That's right.
- The agent models in use: **Gemma 26B** mostly, and Mistral 14B also works. The
  smaller ones struggled, though not since the latest changes. The spec is written for
  the big ones.
- Nothing the person reaches for is missing. The trouble is in how well it does what
  it already does.

## Next, in order

1. ~~Cast looks out of the Image tab's prompts~~: built, awaiting a GPU run.
2. Cast looks out of video prompts. H3: built. LTX 2.5: built, awaiting a GPU run.
   Wan and LTX 2.3: an A/B on each, if wanted.
3. ~~Enhance: a shot's parts go on from each other~~: done 2026-11-23.
4. Steering a running session from the chat (job 3).
5. Suggestions before a dramatic rewrite, with *Just do it* (*Who's in charge*).
6. Offering to draw two pictures when that answers a question (job 1).

## Seen in passing, not yet looked into

From the person's comparison chat on 2026-11-16, four models on "best anatomy":

- **The critique said the penguin and the unicorn "were not in the prompt".** They
  were: they came from `{animal}`. The comparison's critique may not be told what each
  picture's variables rolled.
- **A check phrased the wrong way round was scored the wrong way round.** "Are there
  any visible distortions in the facial features?" was answered *false* (no
  distortions, which is good) and counted as a failure. Every model "failed" it, so
  the table read 0 of 2 everywhere.

---

## Your answers, as written (2026-11-16)

Kept verbatim, so the summaries above can be checked against them.

**What it's for:** Mostly I'm just trying to improve the prompt. Do note, I don't think I've seen the assitant touch the dials much. Also, we default to the standard settings, so a new user is already set to have pretty good results with the right prompt.
What both I and a newcomer would benefit from, is sparring about the image. Currently, I am mostly dictating what I want. But later down the line, if the assistant model can handle it, it would be great to have the feeling for collaboration.

**Asks first / friction:** I think, if the user edits a prompt, e.g. change shirt color to blue, the model should try to respect that unless the user asks otherwise.
Meaning, the current prompt is the source of truth. However, if the user asks to redo the prompt in a dramatic way, the agent can come with some key suggestions, then the user can elaborate or give feedback, or simply press "Just do it".

**Tone:** I don't think it should be too technical. Just concise and friendly. If it is in doubt, ask the user what it wants, perhaps with a short explanation. E.g. I really like it when it asks for the video model. Then it's up to the user to select. And if the user don't know, the assistant has made a recommendation with a short explanation. Perfect!

**Other phrasings:** I think we fixed that pretty well. Only issue, is if I ask for e.g. a 4s clip, and the narrative or action prevents the clip to be done in a 4s timeframe. But that I okay I think.

**1. Draw an example unasked?** I think it could propose creating two images. That would feel interactive and very helpful.

**2. How much rewriting?** Mostly, I think it does really well. Only change I really want is what I wrote about cast under the open questions.

**3. When a session stops short:** Summery would be nice. But here, ad-hoc changes might be handy. I think that could be handled through the chat. Instead of it being locked, it could add more information. THis would also enhance collaboration.

**4. A reason for the winner?** THis is a hard one. And the question is, how much can we expect from the models. However, it would be nice if it could check if anatomy and proportions are correct. But especially for the smaller models, I think it can be a tall order.

**5. Sharing a picture:** I think that works fairly well. It could be refined, but that's not a priority right now. But it would be nice, if I could change the criterias ad-hoc, e.g. I made a session where a knight was figthing a dragon. The images were awesome, but I could see from the picture, that the knight held two shields, and he was looking the wrong way. I would have been nice to be able to highlight those point while the assistant was working.

**6. How far should Enhance go?** I made a note about this in the open questions.

**7. A good film turn:** That's a hard one to answer. What I think is important is that the parts in the shots goes well together. but for several short, consistancy is key. Meaning, same mood, sound, colors etc...

**Open 1, bad at today:**
- Cast is included in prompt. When adding a cast, e.g. Elias, we already have his description when he is added. We don't need that information again in the prompt. This is an issue in both the Enhance button and when assistant writes a prompt. So if the basic prompt is "Elias holding a cat" the prompt should not be " Elias, a weathered cowboy in his late 40s with sun-leathered skin and deep wrinkles ... holding a cat". Focus on what happens in the scene, and things that are not yet defined.
- Video generation when creating a shot. I might define several prompt parts: "Man goes outside", "Man looks up", "Man smokes a cigar". If I enhance these, it will refer to the  first frame. Meaning, the he may "Go outside" several times instead of continueing the story.
- Another issue I found, is that the assitant has a hard time reimagining the short. If always build on the existing prompt, which sometimes becomes a miss match. Also, if I have a complex scene, and ask that one of the character's do something else, the context is lost. I Think this is mostly related to how wordy it gets own cast information is included in the prompt.

**Open 2, can't do:** I haven't really experience noticeable problems here. THat works pretty well.

**Open 3, Image vs Video chat:** Yes, and that's fine. I think that works pretty well.

**Open 4, models:** I work with Gemma 26B, the Mistral 14B also worked pretty well. The smaller one were struggling, but that's okay I think. At least for now, since I haven't tried them with the latest changes.
