import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { test } from "node:test";
import ts from "typescript";

const appUrl = new URL("../src/App.tsx", import.meta.url);
const require = createRequire(appUrl);
const { outputText } = ts.transpileModule(
  `${readFileSync(appUrl, "utf8")}\nexport { clampActivityPosition };`,
  { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX } },
);
const exports = {};
new Function("require", "exports", outputText)(
  (id) => (id.endsWith(".css") ? {} : require(id)),
  exports,
);
const { clampActivityPosition } = exports;

test("progress dock stays inside the window when moved or its available space changes", () => {
  const size = { width: 360, height: 80 };
  const viewport = { width: 1440, height: 900 };
  assert.deepEqual(clampActivityPosition({ x: 400, y: 200 }, size, viewport), { x: 400, y: 200 });
  assert.deepEqual(clampActivityPosition({ x: -500, y: -500 }, size, viewport), { x: 8, y: 8 });
  const bottomRight = clampActivityPosition({ x: 2000, y: 2000 }, size, viewport);
  assert.deepEqual(bottomRight, { x: 1072, y: 812 });
  assert.deepEqual(clampActivityPosition(bottomRight, size, { width: 1100, height: 700 }), { x: 732, y: 612 });
  assert.deepEqual(clampActivityPosition(bottomRight, { width: 560, height: 100 }, viewport), { x: 872, y: 792 });
});
