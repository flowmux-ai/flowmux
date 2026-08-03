// SPDX-License-Identifier: GPL-3.0-or-later

const FILE_NAME_LANGUAGES: Readonly<Record<string, string>> = {
  "cargo.lock": "toml",
  "cargo.toml": "toml",
  "cmakelists.txt": "cmake",
  "dockerfile": "dockerfile",
  "gemfile": "ruby",
  "justfile": "makefile",
  "makefile": "makefile",
  "package-lock.json": "json",
};

const EXTENSION_LANGUAGES: Readonly<Record<string, string>> = {
  bash: "shell",
  c: "c",
  cc: "cpp",
  cfg: "ini",
  clj: "clojure",
  cmake: "cmake",
  cpp: "cpp",
  cs: "csharp",
  css: "css",
  dart: "dart",
  diff: "diff",
  dockerfile: "dockerfile",
  fish: "shell",
  go: "go",
  graphql: "graphql",
  h: "c",
  hpp: "cpp",
  htm: "html",
  html: "html",
  ini: "ini",
  java: "java",
  js: "javascript",
  json: "json",
  jsonc: "json",
  jsx: "javascript",
  kt: "kotlin",
  kts: "kotlin",
  less: "less",
  lua: "lua",
  md: "markdown",
  mjs: "javascript",
  php: "php",
  pl: "perl",
  properties: "ini",
  proto: "protobuf",
  ps1: "powershell",
  py: "python",
  rb: "ruby",
  rs: "rust",
  sass: "scss",
  scss: "scss",
  sh: "shell",
  sql: "sql",
  swift: "swift",
  toml: "toml",
  ts: "typescript",
  tsx: "typescript",
  txt: "plaintext",
  vue: "html",
  xml: "xml",
  yaml: "yaml",
  yml: "yaml",
  zsh: "shell",
};

export function languageForPath(path: string): string {
  const normalized = path.replaceAll("\\", "/");
  const fileName = normalized.slice(normalized.lastIndexOf("/") + 1).toLowerCase();
  const exact = FILE_NAME_LANGUAGES[fileName];
  if (exact !== undefined) {
    return exact;
  }

  const extensionIndex = fileName.lastIndexOf(".");
  if (extensionIndex < 0 || extensionIndex === fileName.length - 1) {
    return "plaintext";
  }
  return EXTENSION_LANGUAGES[fileName.slice(extensionIndex + 1)] ?? "plaintext";
}
