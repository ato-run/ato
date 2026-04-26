// Runtime-agnostic env access (works with Node.js and Deno)
const key =
  (typeof globalThis.Deno !== "undefined")
    ? Deno.env.get("OPENAI_API_KEY")
    : process.env.OPENAI_API_KEY;

if (!key) {
  console.error("Error: OPENAI_API_KEY is not set. Execution blocked.");
  (typeof globalThis.Deno !== "undefined") ? Deno.exit(1) : process.exit(1);
}
console.log("OPENAI_API_KEY is set, length:", key.length);
