import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function loadClaimHarness({ fetchDiscoveredTitleDetails, claimUnidentifiedFile }) {
  const icon = () => React.createElement("span");
  const resolveFile = (base) => {
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = `${base}${suffix}`;
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module from ${base}`);
  };
  const cache = new Map();
  const load = (filename) => {
    if (cache.has(filename)) return cache.get(filename).exports;
    const js = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    const mod = { exports: {} };
    cache.set(filename, mod);
    new Function("require", "module", "exports", js)((name) => {
      if (name === "@radix-ui/react-dropdown-menu") {
        const passthrough = ({ children }) => children ?? null;
        return {
          Root: passthrough,
          Trigger: passthrough,
          Portal: passthrough,
          Content: passthrough,
          Item: ({ children }) => React.createElement("div", null, children),
        };
      }
      if (name === "@/components/artwork-image") {
        return { ArtworkImage: (props) => React.createElement("img", props) };
      }
      if (name === "@/components/icons") {
        return { MoreIcon: icon };
      }
      if (name === "@/lib/api/discover") {
        return {
          titleRef: (source, type, id) => `${source}:${type}:${id}`,
          fetchDiscoveredTitleDetails,
        };
      }
      if (name === "@/lib/api/search") {
        return { searchTitles: async () => ({ items: [] }) };
      }
      if (name === "@/lib/api/libraries") {
        return {
          claimUnidentifiedFile,
          startLibraryScan: async () => {},
        };
      }
      if (name.startsWith("@/")) return load(resolveFile(path.join(webRoot, name.slice(2))));
      if (name.startsWith(".")) return load(resolveFile(path.resolve(path.dirname(filename), name)));
      return require(name);
    }, mod, mod.exports);
    return mod.exports;
  };
  return load(path.join(webRoot, "components/library-detail/unidentified-group-row.tsx"));
}

test("pasting a numeric TMDB ID disables confirm until details resolve and submits resolved title and year", async () => {
  const pendingDetail = deferred();
  const claimed = [];
  const { UnidentifiedGroupRow } = loadClaimHarness({
    fetchDiscoveredTitleDetails: async () => pendingDetail.promise,
    claimUnidentifiedFile: async (payload) => {
      claimed.push(payload);
      return {};
    },
  });

  const group = {
    key: "group-1",
    label: "Inception",
    library_id: "lib-1",
    code: "no_match",
    reason: null,
    file_count: 1,
    total_size_bytes: 1024,
    candidates: [],
    files: [{ id: "f1", file_path: "/movies/Inception.mkv" }],
  };

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(UnidentifiedGroupRow, {
        group,
        movie: true,
        onChanged() {},
      }),
    );
  });

  try {
    const findButton = (text) =>
      renderer.root
        .findAllByType("button")
        .find((b) => (Array.isArray(b.children) ? b.children.join("") : String(b.children ?? "")).includes(text));

    await act(async () => {
      findButton("搜索").props.onClick();
    });

    const input = renderer.root.findByType("input");
    await act(async () => {
      input.props.onChange({ target: { value: "27205" } });
    });

    await act(async () => {
      findButton("查看该 ID").props.onClick();
    });

    const confirmWhileLoading = findButton("认领");
    assert.ok(confirmWhileLoading, "confirm button must be rendered");
    assert.equal(
      confirmWhileLoading.props.disabled,
      true,
      "confirm button must be disabled while TMDB #ID details are still loading",
    );

    await act(async () => {
      pendingDetail.resolve({
        item: {
          id: "27205",
          title: "盗梦空间",
          year: 2010,
          posterUrl: "/posters/27205.jpg",
          extent: "148 分钟",
          rating: 8.8,
          overview: "梦境",
        },
        info: { directors: ["克里斯托弗·诺兰"], cast: [] },
      });
    });

    const confirmReady = findButton("认领");
    assert.equal(confirmReady.props.disabled, false);

    await act(async () => {
      confirmReady.props.onClick();
    });

    assert.equal(claimed.length, 1);
    assert.deepEqual(claimed[0], {
      path: "/movies/Inception.mkv",
      title: "盗梦空间",
      kind: "movie",
      year: 2010,
      tmdb_id: "27205",
    });
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("ClaimConfirmPanel maps resolved year 0 to null on confirm", async () => {
  const claimed = [];
  const { UnidentifiedGroupRow } = loadClaimHarness({
    fetchDiscoveredTitleDetails: async () => ({
      item: {
        id: "999",
        title: "未知年份作品",
        year: 0,
        posterUrl: "",
        extent: "",
        rating: 0,
        overview: "",
      },
      info: { directors: [], cast: [] },
    }),
    claimUnidentifiedFile: async (payload) => {
      claimed.push(payload);
      return {};
    },
  });

  const group = {
    key: "group-2",
    label: "Unknown Year",
    library_id: "lib-1",
    code: "no_match",
    reason: null,
    file_count: 1,
    total_size_bytes: 512,
    candidates: [],
    files: [{ id: "f2", file_path: "/movies/Unknown.mkv" }],
  };

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(UnidentifiedGroupRow, {
        group,
        movie: true,
        onChanged() {},
      }),
    );
  });

  try {
    const findButton = (text) =>
      renderer.root
        .findAllByType("button")
        .find((b) => (Array.isArray(b.children) ? b.children.join("") : String(b.children ?? "")).includes(text));
    await act(async () => { findButton("搜索").props.onClick(); });
    await act(async () => { renderer.root.findByType("input").props.onChange({ target: { value: "999" } }); });
    await act(async () => { findButton("查看该 ID").props.onClick(); });
    await act(async () => { findButton("认领").props.onClick(); });
    assert.equal(claimed.length, 1);
    assert.equal(claimed[0].title, "未知年份作品");
    assert.equal(claimed[0].year, null);
  } finally {
    await act(async () => { renderer.unmount(); });
  }
});
