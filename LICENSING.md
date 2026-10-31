# Licensing

Copyright (C) 2026 Nathan Davis.

## The code: GPL-3.0-or-later

Engines (the engine, the live player, the authoring tools, the VS Code extension, the skills, the lab, the
Swift shell and the contracts) is free software. You can redistribute and modify it under the terms of the
GNU General Public License, version 3 or (at your option) any later version. The text is in
[`LICENSE`](LICENSE), and every manifest says `GPL-3.0-or-later`.

Forks and derivatives stay open under the same licence.

## Dual licensing and the CLA

The copyright holder also licenses this code to their own closed products under other terms: a DAW plugin
and a companion extension, Engines Studio. That's the dual licence. So that a contribution can ship in both,
every outside contribution comes with a signed contributor licence agreement ([`CLA.md`](CLA.md)): you keep
the copyright on what you write, and grant the project a licence to use it under the GPL and under other
terms. The CLA check on your first pull request links to it.

## Third-party code

`make audit` (cargo-deny, [`deny.toml`](deny.toml)) allows only permissive and weak-copyleft licences for
third-party crates: no GPL, AGPL or LGPL-only crate anywhere. The closed products link these crates, so a
GPL dependency here would also ship in closed code. The copyright holder's own crates are exempt by name.
The extension's runtime npm packages are held to the same rule by `.github/scripts/licences.js`.
