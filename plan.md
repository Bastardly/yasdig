# Feature 1 - Prompt strength keyboard short cuts
In apps like ComfyUI and Forge, we are able to increase the strength of a given part of the prompt.
I don't remember the format completely. I think it is something like "A lovely [cat:1,2] sitting in a window" where the power of the word "cat" is increased to 120%. Whatever it is, it should follow the same format that Comfy and Forge uses.

The way it works in forge, is that I can either select a part of the text with my mouse, or I can have the mouse curser inside a word.
Then I can hold down ctrl and either use the mouse wheel or up and down key to modify the word's or selection's strength by 0.1. If the strength is 1, then the format wrapper disappear, and the normal text is shown.

# Change request 1 - Move generate button
Currently, the generate button and associated status info and stop buttons are at the bottom of the menu. I would like to move it to the top, so that it's always the first item in the "card col-control" for all tabs. Reason is that if I generate multiple images, I'll have to scroll up and down, and I even have a large screen. So moving it to the top will be very helpful.

# Feature 2 - User settings
Currently we have the models.toml where we set the default settings. Instead, it would be nice to have a ./user/settings.json (A pattern I intend on expanding) and a settings tab.

Currently, I think the settings could be boiled down to:
- Default CFG scale
- Default Steps
- Default width and height
- Default refine steps and strength
- Default high-res scale, steps and strength

Changing the setting should simply work like a normal form, with a save button instead of a generate.

# Feature 3 - Favorite work items
Under the image tab, we have the work items section which lists all our current work items. Problem is that this quickly becomes very large, which means that I often use the "delete all" function.
But I would like to be able to save some work items as favorites that is saved under ./user/favorite (not created).

There are two parts to this.
1) Be able to mark a work item as a favorite. I think we could borrow the UX pattern from delete, where we choose a "Click to favorite" (Adjust grammer if needed) radio button. Click the work items we wish to save, highlight them green instead of red, and save to favorite. Once that happens, a copy is made under the ./user/favorite folder.
2) We need to be able to show our favorites. So I was thinking two buttons at the top of the workitems card: "Current" and "Favorites". 
And all that really should do, is to toggle between the two souce folders. Only difference, is that while on the Favorites folder, we should not show the "Click to favorite", since adding favorites to favorites wouldn't make sense. Also the delete all button shouldn't be shown either in favorite mode.

Of course, the delete and delete all should only influence the selected source folder.
