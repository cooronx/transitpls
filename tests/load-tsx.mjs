import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import ts from "typescript";

const cache = new Map();

// Load local TSX imports too, so rendering tests exercise the actual components.
export function loadTsx(url, extraExports = "") {
  const key = `${url.href}:${extraExports}`;
  if (cache.has(key)) return cache.get(key);
  const require = createRequire(url);
  const exports = {};
  cache.set(key, exports);
  const { outputText } = ts.transpileModule(
    `${readFileSync(url, "utf8")}\n${extraExports}`,
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
      },
    },
  );
  new Function("require", "exports", outputText)((id) => {
    if (id.endsWith(".css")) return {};
    if (id.endsWith(".json")) return require(id);
    if (id.startsWith(".")) return loadTsx(new URL(`${id}.tsx`, url));
    return require(id);
  }, exports);
  return exports;
}
