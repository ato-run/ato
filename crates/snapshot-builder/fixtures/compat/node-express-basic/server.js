const express = require("express");
const app = express();
app.get("/health", (_req, res) => res.send("ok"));
app.get("/", (_req, res) => res.send("compat-node-express-basic"));
app.listen(8080, "0.0.0.0");
