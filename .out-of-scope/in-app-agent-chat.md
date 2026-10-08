# In-app agent chat

Canvas does not host a chat with an agent inside its window.

## Why this is out of scope

A chat panel that drives an agent (Television does this over the Agent Client Protocol, spawning the agent as a child process) turns Canvas into an agent development environment: it would own agent processes, sessions, permissions and transcripts. Canvas is an external tool for working out the UX of showing agent output, meant to be folded into a fuller agent environment later, not to become one. Agents reach Canvas through the `canvas` CLI from whatever harness they already run in.

## Prior requests

- Compared against Television's ACP bridge during the Canvas Artifacts and Timeline spec (canvas-vv9)
