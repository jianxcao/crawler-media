import assert from "node:assert/strict";
import { createServer as createHttpServer } from "node:http";
import test from "node:test";
import { createServer } from "vite";

test("Vite forwards same-origin API requests to the configured backend", async () => {
  const backend = createHttpServer((request, response) => {
    response.end(request.url);
  });
  await new Promise((resolve) => backend.listen(0, "127.0.0.1", resolve));
  const previousTarget = process.env.VITE_BACKEND_TARGET;
  process.env.VITE_BACKEND_TARGET = `http://127.0.0.1:${backend.address().port}`;
  let vite;
  try {
    vite = await createServer({ server: { port: 0, strictPort: false }, logLevel: "silent" });
    await vite.listen();
    const address = vite.httpServer.address();
    const response = await fetch(`http://127.0.0.1:${address.port}/api/v1/auth/me`);
    assert.equal(await response.text(), "/api/v1/auth/me");
  } finally {
    await vite?.close();
    await new Promise((resolve, reject) => backend.close((error) => error ? reject(error) : resolve()));
    if (previousTarget === undefined) delete process.env.VITE_BACKEND_TARGET;
    else process.env.VITE_BACKEND_TARGET = previousTarget;
  }
});
