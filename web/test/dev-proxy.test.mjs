import assert from "node:assert/strict";
import { createServer as createHttpServer, get as httpGet } from "node:http";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { createServer } from "vite";

const WEB_ROOT = fileURLToPath(new URL("..", import.meta.url));

function requestStatus(port, hostHeader, path = "/") {
  return new Promise((resolve, reject) => {
    const req = httpGet(
      {
        host: "127.0.0.1",
        port,
        path,
        agent: false,
        headers: { Host: hostHeader, Connection: "close" },
      },
      (res) => {
        res.resume();
        res.once("end", () => resolve(res.statusCode));
      },
    );
    req.once("error", reject);
  });
}

async function withEnv(overrides, fn) {
  const previous = new Map();
  for (const [key, value] of Object.entries(overrides)) {
    previous.set(key, process.env[key]);
    if (value === undefined) delete process.env[key];
    else process.env[key] = value;
  }
  try {
    return await fn();
  } finally {
    for (const [key, value] of previous.entries()) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
  }
}

async function createTestViteServer() {
  const vite = await createServer({
    root: WEB_ROOT,
    server: { port: 0, strictPort: false },
    logLevel: "silent",
  });
  await vite.listen();
  return vite;
}

async function closeViteServer(vite) {
  vite.httpServer?.closeAllConnections?.();
  await vite.close();
}

test("Vite forwards same-origin API requests and rejects unlisted Host when CM_DEV_ALLOWED_HOSTS is unset", async () => {
  const backend = createHttpServer((request, response) => {
    response.end(request.url);
  });
  await new Promise((resolve, reject) => {
    backend.once("error", reject);
    backend.listen(0, "127.0.0.1", () => {
      backend.off("error", reject);
      resolve();
    });
  });
  try {
    await withEnv(
      {
        VITE_BACKEND_TARGET: `http://127.0.0.1:${backend.address().port}`,
        CM_DEV_ALLOWED_HOSTS: undefined,
      },
      async () => {
        const vite = await createTestViteServer();
        try {
          const { port } = vite.httpServer.address();
          const response = await fetch(`http://127.0.0.1:${port}/api/v1/auth/me`);
          assert.equal(await response.text(), "/api/v1/auth/me");

          assert.equal(await requestStatus(port, `127.0.0.1:${port}`), 200);
          assert.equal(await requestStatus(port, "evil.example"), 403);
        } finally {
          await closeViteServer(vite);
        }
      },
    );
  } finally {
    backend.closeAllConnections?.();
    await new Promise((resolve, reject) =>
      backend.close((error) => (error ? reject(error) : resolve())),
    );
  }
});

test("Vite rejects non-localhost Host when CM_DEV_ALLOWED_HOSTS is empty", async () => {
  await withEnv({ CM_DEV_ALLOWED_HOSTS: " , " }, async () => {
    const vite = await createTestViteServer();
    try {
      const { port } = vite.httpServer.address();
      assert.equal(await requestStatus(port, `127.0.0.1:${port}`), 200);
      assert.equal(await requestStatus(port, `mac-mini.local:${port}`), 403);
      assert.equal(await requestStatus(port, "evil.example"), 403);
    } finally {
      await closeViteServer(vite);
    }
  });
});

test("Vite allows CM_DEV_ALLOWED_HOSTS entries while rejecting unlisted Host headers", async () => {
  await withEnv(
    { CM_DEV_ALLOWED_HOSTS: "mac-mini.local, 192.168.1.50" },
    async () => {
      const vite = await createTestViteServer();
      try {
        const { port } = vite.httpServer.address();
        assert.equal(await requestStatus(port, `127.0.0.1:${port}`), 200);
        assert.equal(await requestStatus(port, `localhost:${port}`), 200);
        assert.equal(await requestStatus(port, "mac-mini.local"), 200);
        assert.equal(await requestStatus(port, `mac-mini.local:${port}`), 200);
        assert.equal(await requestStatus(port, `192.168.1.50:${port}`), 200);
        assert.equal(await requestStatus(port, "evil.example"), 403);
      } finally {
        await closeViteServer(vite);
      }
    },
  );
});

test("Vite normalizes mixed-case CM_DEV_ALLOWED_HOSTS entries for lowercase browser Host headers", async () => {
  await withEnv({ CM_DEV_ALLOWED_HOSTS: "Mac-Mini.local" }, async () => {
    const vite = await createTestViteServer();
    try {
      const { port } = vite.httpServer.address();
      assert.equal(await requestStatus(port, "mac-mini.local"), 200);
      assert.equal(await requestStatus(port, `mac-mini.local:${port}`), 200);
      assert.equal(await requestStatus(port, "evil.example"), 403);
    } finally {
      await closeViteServer(vite);
    }
  });
});
