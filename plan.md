# ~~Feature 1 - Prompt strength keyboard short cuts~~ — DONE
Ctrl+↑/↓ and Ctrl+wheel on any of the six prompt boxes, in steps of 0.1, with the
wrapper removed again at 1.0. The syntax is `(cat:1.2)` — round parens, dot decimal —
which is what Comfy, Forge *and* sd.cpp's own parser use; `[cat:1,2]` would not have
errored, it would have been conditioned as literal text at weight 0.909.
`web/js/prompt-weight.js` + `web/js/prompt-box.js`; see *## The prompt box* in
CLAUDE.md and *### Prompt weighting* in README.md.

# ~~Change request 1 - Move generate button~~ — DONE
The button, its Stop, the progress line and the panel's errors are now a `.gohead`
block that is the first child of `card col-controls` on all four tabs. The image
tab's *run* panel (Images, the checkboxes) deliberately stayed at the foot: those
are set once a session and the prompt is what belongs under the button.

# ~~Feature 2 - User settings~~ — DONE
`user/settings.toml` (TOML, not JSON — one parser, and it sits beside models.toml)
plus a Settings tab that edits it as a form. `[defaults]` is gone from models.toml,
which is now bootstrap config only; a `[templates.*]` block became a *partial*
override, so it states what is different about one checkpoint and inherits the
rest from your settings. Includes the default prompt. Video deliberately left out.
See *## Settings* in CLAUDE.md and *### Settings* in README.md.

# Feature 3 - Favorite work items
Under the image tab, we have the work items section which lists all our current work items. Problem is that this quickly becomes very large, which means that I often use the "delete all" function.
But I would like to be able to save some work items as favorites that is saved under ./user/favorite (not created).

There are two parts to this.
1) Be able to mark a work item as a favorite. I think we could borrow the UX pattern from delete, where we choose a "Click to favorite" (Adjust grammer if needed) radio button. Click the work items we wish to save, highlight them green instead of red, and save to favorite. Once that happens, a copy is made under the ./user/favorite folder.
2) We need to be able to show our favorites. So I was thinking two buttons at the top of the workitems card: "Current" and "Favorites". 
And all that really should do, is to toggle between the two souce folders. Only difference, is that while on the Favorites folder, we should not show the "Click to favorite", since adding favorites to favorites wouldn't make sense. Also the delete all button shouldn't be shown either in favorite mode.

Of course, the delete and delete all should only influence the selected source folder.

# Feature 4 - User defined prompt variables.
I would like to create a new section under the settings tab called variables.

Here we should be able to add, edit and remove words, with words attached. E.g.
"animal": ["crab", "horse", "dog", "cat"],
"color": ["red", "blue", "yellow"],

The variables should be saved like any other setting.
The array could be defined as a simple like of text. e.g. "red, blue, yellow" or "glorious gopher, pretty python, creative crab" - allowing for spaces

If people write jibberish, e.g. ",,,,," that's their problem. But do add validation on save if needed, if it can break the app.


Goal is to be able to use this in a prompt using a format - I don't know if there is a standard for this. But here is an example:
"A lovely [animal] wearing a [color] hat"
So that every time I run the prompt, a random generation is made. E.g. "A lovely crab wearing a blue hat"
