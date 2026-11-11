// The hello plugin: JSON-RPC 2.0 over stdio, one message per line. No dependencies.
"use strict";

const readline = require("readline");

let nextId = 1;
const waiting = new Map();

function send(message) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...message }) + "\n");
}

// Calls the IDE and resolves with its answer.
function call(method, params) {
  const id = nextId++;
  send({ id, method, params });
  return new Promise((resolve, reject) => waiting.set(id, { resolve, reject }));
}

function log(message) {
  send({ method: "log", params: { message } });
}

const commands = {
  async "upper-case"() {
    const document = await call("editor/activeDocument");
    if (!document) {
      await call("window/showMessage", { message: "Hello: no editor is open", kind: "warning" });
      return;
    }
    if (document.selectedText === "") {
      await call("window/showMessage", { message: "Hello: select some text first" });
      return;
    }
    await call("editor/replaceSelection", { text: document.selectedText.toUpperCase() });
  },
};

async function handle(request) {
  switch (request.method) {
    case "initialize":
      log(`hello started, api ${request.params.api}`);
      return {};
    case "command/execute": {
      const command = commands[request.params.command];
      if (!command) throw new Error(`no command ${request.params.command}`);
      await command();
      return null;
    }
    case "shutdown":
      return null;
    default:
      throw { code: -32601, message: `no method ${request.method}` };
  }
}

readline.createInterface({ input: process.stdin }).on("line", (line) => {
  if (line.trim() === "") return;
  const message = JSON.parse(line);
  if (message.method === "exit") process.exit(0);
  if (message.method === undefined) {
    // An answer of the IDE to one of our calls.
    const pending = waiting.get(message.id);
    waiting.delete(message.id);
    if (!pending) return;
    if (message.error) pending.reject(new Error(message.error.message));
    else pending.resolve(message.result);
    return;
  }
  handle(message).then(
    (result) => send({ id: message.id, result: result ?? null }),
    (error) => send({ id: message.id, error: { code: error.code ?? -32000, message: String(error.message ?? error) } }),
  );
});
