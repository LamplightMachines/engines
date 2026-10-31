# Security

Please report a security problem privately, through GitHub's vulnerability reporting:
[open a report](https://github.com/LamplightMachines/engines/security/advisories/new). Don't open a public
issue for it.

What's in scope: the engine and its tools read loop documents (JSON5) and write audio files, the live player
reads a piece's folder and talks JSON lines over stdio, and the VS Code extension starts those tools. A doc
that crashes a tool, writes outside the piece's folder, or makes the extension run something it shouldn't is
worth a report. Nothing here runs as a network service, and there's no telemetry.

You'll get an answer within a week. Fixes go into the next release, with credit if you'd like it.
