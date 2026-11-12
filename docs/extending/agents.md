# AI agents

An AI agent - a coding assistant or any other program that speaks the Model Context Protocol - can work with your
project alongside you.

## Connecting an agent

Add the editor's command line to your agent as a local MCP server, with the project's folder:

```
pe2-cli mcp --project path/to/project
```

The agent can then find assets, read them, follow their references, list problems and look at previews, whether
or not the editor is open.

## With the editor open

While the editor has the project open, the agent works through it:

- its changes are steps in your [history](../working/history.md), and can be undone like yours;
- it can always see the editor's window;
- it can **use** the window - click, type, run commands - only if you allow it, in **Settings ▸ Agents**.

The status bar shows when an agent is using the editor.

An agent cannot answer the questions a project asks when it is first opened, such as what to trust. Those are
yours.

## After updating the editor

Restart the agent's connection to the editor after an update, so that it runs the new version.
