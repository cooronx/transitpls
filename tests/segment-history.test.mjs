import assert from "node:assert/strict";
import { test } from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { loadTsx } from "./load-tsx.mjs";

const { ChangedText } = loadTsx(
  new URL("../src/SegmentCard.tsx", import.meta.url),
);

test("version comparison preserves complete Unicode text and escapes markup", () => {
  const render = (value, other, addition = true) =>
    renderToStaticMarkup(
      createElement(ChangedText, { value, other, addition }),
    );
  assert.equal(render("前😀后", "前😁后"), "前<ins>😀</ins>后");
  assert.equal(render("前旧文后", "前后", false), "前<del>旧文</del>后");
  assert.equal(render("相同", "相同"), "相同");
  assert.equal(render("<script>", ""), "<ins>&lt;script&gt;</ins>");
  assert.equal(render("", "删除"), "");
});
