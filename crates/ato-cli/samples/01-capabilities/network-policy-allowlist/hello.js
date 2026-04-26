// Uses fetch() — available in Node.js 18+ and Deno natively
fetch("https://httpbin.org/get")
  .then((res) => res.json())
  .then((data) => {
    console.log("httpbin response origin:", data.origin);
  })
  .catch((err) => {
    console.error("Request failed:", err.message);
    (typeof globalThis.Deno !== "undefined") ? Deno.exit(1) : process.exit(1);
  });
