# Feature - Backend job handling

Currently the we are taking one job at a time. If we navigate away from the generation tab, there is a good chance that some data is lost.
Also, we cannot plan multiple jobs

Therefore, it would be great to have some state handling on the backend, where the user can send multiple job, which is listed on a new "Jobs" tab, which should be placed as the last tab after setting. (Default open tab should still be the generate tab)
The Jobs tab should also display how many jobs are currently listed. E.g.: Jobs (3)

If there are 0 jobs, the jobs tab should not be visible
If there are any jobs, the jobs tab should be visible as a green button (Should probably be added to css variables. I'm thinking same saturation and lightness as the close server button - only en in green hue)

Once a job is done, it is removed from the jobs list, as saved as stated in the workflow, and the workflow is added.

If there is an active job that has the "Keep going until stopped" setting active, the jobs button should also show a red exclamation mark (icon) after the count: Jobs (3)!

If the Jobs tab is clicked, it should not navigate to a new tab, but instead open a narrow dialog, showing a list of awaiting jobs.
The list should consist of generation type, model name, progress: e.g. (4/8) or (Running until stopped)
In this dialog, it should be possible to remove a job by clicking a red "Stop" or "Remove" button on. The button should either stop the generation if active, like the current "stop" button, or "remove" the job from the list. On stop/remove, there should be a confirm dialog.
Clicking outside the dialog should close it, but not select anything else while the dialog is open. Consider the inert option - but you decide.

## Related change request - Cleanup old code/logic
I see we may need to clean up the code. Every time we currently generate an image, a temporaty workflow is placed in the root of the work folder. We have a buffer of I think 8 jobs. This was the initial solution and can be removed, since we have workflows saved in work/workitems.

## Related change request - Allow site navigation when jobs are in progress
- Should be able to navigate between tabs. E.g. if I make changes in the generation tab, and navigate to outputs tab, and the navigate back. The changes I made before should persist and not default back to default. Defaults should only be used on initial load or model change. (I think this is already the case - but worth checking afterwards)
- Should be able to edit outputs while jobs are active. (I can navigate to outputs while an image is being generated, but I cannot open the edit dialog)
