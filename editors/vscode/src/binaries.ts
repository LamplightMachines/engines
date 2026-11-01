// Where the tools live: a companion's swapped-in build (the companion API), else a configured path
// (absolute, or relative to the repo root), else the workspace's `target/release/`. Missing ⇒ in a
// checkout, offer to build them in a terminal task; anywhere else, say to reinstall ([`missingTool`]). A
// companion resolves its own tools the same way ([`resolveBinary`]).

import * as fs from 'node:fs';
import * as path from 'node:path';
import * as vscode from 'vscode';
import { missingTool } from './missing';

/** This extension's own tools. */
export type Tool = 'engines-lsp' | 'engines-live';

/** How to find and build a tool: its binary's name, its `engines.*` path setting, its cargo build. */
export interface BinarySpec {
  name: string;
  setting: string;
  build: string;
}

const SPECS: Record<Tool, BinarySpec> = {
  'engines-lsp': { name: 'engines-lsp', setting: 'server.path', build: 'cargo build --release -p engines_authoring --bin engines-lsp' },
  'engines-live': { name: 'engines-live', setting: 'live.path', build: 'cargo build --release -p engines_live --bin engines-live' },
};

/** The builds companions swapped in (`setBinary`), by tool. */
const swapped = new Map<Tool, string>();

/** A companion's build for `tool` (`undefined`: this extension's own again). */
export function swapBinary(tool: Tool, file: string | undefined): void {
  if (file) {
    swapped.set(tool, file);
  } else {
    swapped.delete(tool);
  }
}

/** The repo root: the first workspace folder with the authoring crate (else with `pieces/`, else the first). */
export function repoRoot(): string | undefined {
  const folders = vscode.workspace.workspaceFolders?.map((f) => f.uri.fsPath) ?? [];
  return (
    folders.find((f) => fs.existsSync(path.join(f, 'crates', 'engines_authoring'))) ??
    folders.find((f) => fs.existsSync(path.join(f, 'pieces'))) ??
    folders[0]
  );
}

/** The binary `spec` names: its configured path, else `target/release/` in `root`; `undefined` when it
 *  isn't there. */
export function resolveBinary(spec: BinarySpec, root: string | undefined): string | undefined {
  const configured = vscode.workspace.getConfiguration('engines').get<string>(spec.setting, '').trim();
  const candidates = configured
    ? [path.isAbsolute(configured) || !root ? configured : path.join(root, configured)]
    : root
      ? [path.join(root, 'target', 'release', spec.name)]
      : [];
  return candidates.find((p) => fs.existsSync(p));
}

/** This extension's `tool`: a companion's build of it first. */
export function resolveTool(tool: Tool, root: string | undefined): string | undefined {
  const own = swapped.get(tool);
  return own && fs.existsSync(own) ? own : resolveBinary(SPECS[tool], root);
}

/** Tell the user `tool` is missing; in a checkout, offer to build it. Resolves `true` once a build succeeded. */
export function offerBuild(tool: Tool, root: string | undefined): Promise<boolean> {
  return offerBuildOf(SPECS[tool], root);
}

/** [`offerBuild`] for any tool `spec` describes (a companion's). */
export async function offerBuildOf(spec: BinarySpec, root: string | undefined): Promise<boolean> {
  const missing = missingTool(spec.name, root);
  const pick = await vscode.window.showWarningMessage(missing.text, ...(missing.build ? ['Build it'] : []), 'Settings');
  if (pick === 'Settings') {
    await vscode.commands.executeCommand('workbench.action.openSettings', `engines.${spec.setting}`);
    return false;
  }
  if (pick !== 'Build it' || !missing.build || !root) {
    return false;
  }
  return runBuild(spec, root);
}

/** Run the cargo build in a terminal task; resolves with its success. */
export async function runBuild(spec: BinarySpec, root: string): Promise<boolean> {
  const folder = vscode.workspace.getWorkspaceFolder(vscode.Uri.file(root)) ?? vscode.TaskScope.Workspace;
  const task = new vscode.Task({ type: 'shell', tool: spec.name }, folder, `build ${spec.name}`, 'engines', new vscode.ShellExecution(spec.build, { cwd: root }));
  task.presentationOptions = { reveal: vscode.TaskRevealKind.Always, clear: true };
  const exec = await vscode.tasks.executeTask(task);
  return new Promise<boolean>((resolve) => {
    const sub = vscode.tasks.onDidEndTaskProcess((e) => {
      if (e.execution === exec) {
        sub.dispose();
        resolve(e.exitCode === 0);
      }
    });
  });
}
