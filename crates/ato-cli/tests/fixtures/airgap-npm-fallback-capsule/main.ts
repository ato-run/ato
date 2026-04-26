import { chunk } from "npm:lodash-es@4.17.21";

console.log(
  "airgap cached-only run OK",
  JSON.stringify(chunk([1, 2, 3, 4], 2)),
);
