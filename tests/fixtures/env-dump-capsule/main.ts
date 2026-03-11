const env = Deno.env.toObject();
console.log(JSON.stringify(env));

const fd = env["ATO_SECRET_FD_OPENAI_API_KEY"];
if (!fd) {
  console.error("missing secret FD mapping");
  Deno.exit(41);
}

console.log(`secret fd: ${fd}`);
