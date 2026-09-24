import assert from "node:assert/strict";
import { test } from "node:test";
import { loadTsx } from "./load-tsx.mjs";

const { countTranslationRequests } = loadTsx(
  new URL("../src/App.tsx", import.meta.url),
  "export { countTranslationRequests };",
);

const segment = (status, target) => ({ source: "s", status, target });
const chapter = (id, title, target_title, segments) => ({
  id,
  title,
  target_title,
  segments,
});

// 与后端一致：正文已完成的章节才计入标题批次。
test("title batches follow chapter body completion", () => {
  const chapters = [
    chapter("a", "A", null, [segment("translated", "甲")]),
    chapter("b", "B", null, [segment("pending", null)]),
    chapter("c", "C", "丙", [segment("pending", null)]),
  ];
  assert.equal(countTranslationRequests(chapters, 1), 4);
  assert.equal(countTranslationRequests(chapters, 1, "b"), 3);
  assert.equal(countTranslationRequests(chapters, 1, "a"), 1);
});
