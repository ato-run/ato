// Runtime-agnostic env access (works with Node.js and Deno)
const msg =
  (typeof globalThis.Deno !== "undefined")
    ? Deno.env.get("APP_MESSAGE")
    : process.env.APP_MESSAGE;

console.log("APP_MESSAGE:", msg || "(not set)");
