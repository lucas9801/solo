# Reviewing documents agents write

[← Home](../index.md)

A lot of work now starts with an agent writing a plan, a design or a research report: you read it, comment, it revises, then it builds. Solo keeps that loop in one place.

## Opening

- When an agent's output mentions a path like `PLAN.md`, click it to open a **rendered preview** on the right, not the source.
- Mermaid diagrams render in the preview and can be zoomed. Clicking a `mermaid` line in an agent's output shows the diagram too; when the screen only shows part of it, Solo takes the whole diagram from Claude Code's transcript.
- Images open in the image viewer, code in the editor.

## Commenting

1. Select text in the preview, right-click and choose **Comment…**, and write your comment.
2. Comments are listed under the document, where they can be edited and deleted.
3. Click **Send to Agent**: all the comments are pasted into the agent as one message, each with its lines, the text it's on and what you said.

Which agent: the one that changed the document in a turn, if one did; otherwise the one you used last.

## When the document changes again

If an agent changes the document after you read it, a bar above the preview says **how many lines were added and removed**. **Show Changes** marks the changed blocks; **Mark as Read** takes it as read again.

So you read only what changed, not the whole document again.
