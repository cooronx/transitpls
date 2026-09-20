import assert from "node:assert/strict";
import { test } from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { loadTsx } from "./load-tsx.mjs";

const { Explorer } = loadTsx(new URL("../src/App.tsx", import.meta.url), "export { Explorer };");

const project = {
  id: "book",
  title: "Current title",
  source_file: "book.epub",
  status: "translated",
  chapters_total: 2,
  chapters_completed: 2,
};
const cover = "data:image/png;base64,iVBORw0KGgo=";

function renderExplorer(projects) {
  return renderToStaticMarkup(
    createElement(Explorer, {
      projects,
      detail: { project, chapters: [] },
      chapterIndex: 0,
      onChapter() {},
      onProject() {},
      onImport() {},
    }),
  );
}

test("sidebar uses the matching summary cover and the current project details", () => {
  const html = renderExplorer([
    { ...project, id: "other", cover_data_url: "other-cover.png" },
    {
      ...project,
      title: "Outdated title",
      chapters_completed: 0,
      cover_data_url: cover,
    },
  ]);
  assert.ok(
    html.includes(`src="${cover}"`),
    "sidebar should render the book cover",
  );
  assert.ok(html.includes("Current title"));
  assert.ok(html.includes("100%"));
  assert.ok(!html.includes("Outdated title"));
});

test("sidebar keeps the placeholder when no summary cover is available", () => {
  for (const projects of [[], [{ ...project, cover_data_url: null }]]) {
    assert.ok(
      renderExplorer(projects).includes('<span class="cover-sm">文</span>'),
    );
  }
});
