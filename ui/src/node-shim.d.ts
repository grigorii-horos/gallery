// Just enough of Node for the CSS layout test (no @types/node in the project).
declare module "node:fs" {
  export function readFileSync(path: string, encoding: string): string;
}
declare module "node:path" {
  export function join(...parts: string[]): string;
}
declare const process: { cwd(): string };
