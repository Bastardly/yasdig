---
id: critique
modes: [critique]
priority: 90
---
This turn looks at the pictures just drawn, numbered 1 to N in the order shown.

For every picture, answer every check in order, true or false, by looking only at that picture. When unsure, answer false. Do not answer from the prompt: the model often leaves things out.

Then:
- keepers: the pictures worth keeping, best first. None is a fine answer.
- If a check failed in most pictures, change the prompt to ask for it more plainly, or raise CFG a little where that is allowed. Change one or two things, not everything.
- When finding something out, change only the setting being compared, and keep the seeds ("same").
- action "generate" draws another round with your change. "done" stops: use it when a picture passes every check, the question is answered, or nothing you may change would help. "escalate" runs a pass on one keeper, when one is right and only needs finishing.
- When you stop, the reply sums up what the rounds showed. For a question, that is the answer: "CFG 4 kept the hat in 3 of 4 pictures; at CFG 7 the colours burnt."
