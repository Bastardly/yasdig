# Next steps


# 1 BUG/Change Request: Scale factor is reset on every image
Currently, in the UI, the scale factor select is reset when creating a new image. 
I would like it to persist whatever we selected.
It has id="upfactor"

# 2 Feature(1/4): Layout changes - Workspace container for active work items.
This is the first part of a larger feature.
Plan here is to change layout of the UI.

Currently. Layout is set like this:
layout {
    grid-template-columns: minmax(340px, 420px) 1fr;
  }

1) Align the .card all the way to the left of the screen.
2) .result should be placed left to it.
3) Below .card and .result, add a new container .workitems for the future feature. This should be full width of the screen.

On smaller screens, the .result should be placed under .card.

# 3 Feature(2/4): Save work items to disk.
## The issue
Right now, when we generate an image, it is saved to the ./work folder, and it automatically cleaned up.
If I generate a new image, I cannot go back to the previous image and continue the work. Also I cannot batch render a bunch of images (Later feature), select the best one and run that through refinement etc.

Also, we have a cleanup that automatically removes the images, which should be removed once items are saved to disk.

Therefore, it would be awesome, if the generated image was saved to disk. E.g. ./work/workitems. Not only is the image saved, but also its settings, which we will use in the next feature.

## Subfeature: Save to output
Refined and highres images should be saved to ./work/output. This way, I can do multiple refinements on the same image. I'm thinking it is an option that the user can do, by clicking a button above the image "Save to output". Otherwise, it won't be saved.

# 4 Feature(3/4): Display work items in the work area.
Here I imagine a grid of images. The images can be of various sizes, but they are contrained to 200px in height. There should only be a very narrow gap between each image. Width is relative to the original image. All images should act like buttons. 

Above the images, there should be a toggle. Where the user can choose between "Click to open"(default), and "Click to delete".


# 5 Feature(4/4): Handle click logic in workitems.
When toggle is on "Click to delete" and the user clicks a thumbnail in the workitems, the workitem should be deleted from disk, and the thumbnail should be removed.

When the toggle is on "Click to open" and the user clicks a thumbnail in the workitems, the workitem's settings should be applied to .card and the image should be shown in .result
