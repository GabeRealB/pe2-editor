# Agents

`crates/mcp`, `pe2-cli mcp`, the editor's `drive.rs`.

## The path of a request

An agent talks to `pe2-cli mcp` over the Model Context Protocol, on standard input and output. Every request is a
value (`mcp::Request`: a tool call, a resource read, an edit, a signal) and one function answers it against an index
(`mcp::answer`). Who answers depends on who holds the project:

1. If an editor or a host holds it, the request is forwarded to it over the **holder's link**: a local socket in the
   project's cache, one CBOR message per frame.
2. If nothing holds it, a host is started and the request forwarded to it.
3. Where no host can be started, the server takes the project lock for that request alone and answers itself.

So an agent's session never keeps the editor from opening the project, and only one process touches the project at a
time. The first request asks who answers, and a host from an older build is replaced.

## In the editor

The link has its own thread, and answers without a frame wherever it can:

- reads are answered against the project as last handed over, once the worker has taken in what was asked before,
  so an agent reads what it just wrote;
- edits, what programs did through the folder, and signals go to the worker, like the window's own;
- only what is about the window itself - its widgets, a screenshot, acting in it - waits for a frame.

The link starts listening before the project is read, so agents can reach the window while it opens.

## Driving the window

The editor's agent driver is an egui plugin. It keeps the window's accessibility tree while agents are asking (the
`editor_ui` tool lists widgets from it), injects input events into frames (`editor_act`, `editor_type`), and answers
each request once the window has settled, with what changed. Commands go through the same `Command` path as the
palette. Acting in the window requires the user's permission; what an agent does is recorded as the agent's, and is
undoable. Questions only a person may answer, such as what to trust, refuse agents.

## Presence

Each editor writes a small presence file in the user's data folder, refreshed while it runs. The `editors` tool reads
these to say what is running and who holds which project, without touching any project.

## Where to look

`mcp::Request`, `mcp::answer`, `mcp::server` (forwarding, starting a host), `mcp::link`, `mcp::presence`,
`mcp::tools`; the editor's `drive.rs` and `takein::Outside`; the host's request loop in `cli/src/host.rs`.
