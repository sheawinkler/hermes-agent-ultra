#!/usr/bin/env node
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";

const repoRoot = resolve(new URL("..", import.meta.url).pathname);
const indexPath = join(repoRoot, "website", "static", "api", "skills-index.json");
const maxAgeHours = Number(process.env.SKILLS_INDEX_MAX_AGE_HOURS || "26");
const minTotal = Number(process.env.SKILLS_INDEX_MIN_TOTAL || "1500");
const floors = {
  "skills.sh": 100,
  lobehub: 100,
  clawhub: 50,
  official: 50,
  github: 30,
  "browse-sh": 50,
};

function fail(message) {
  console.error(`[skills-index] ${message}`);
  process.exitCode = 1;
}

let data;
try {
  data = JSON.parse(readFileSync(indexPath, "utf8"));
} catch (err) {
  fail(`cannot read ${indexPath}: ${err.message}`);
  process.exit();
}

const skills = Array.isArray(data.skills) ? data.skills : null;
if (!skills) {
  fail("invalid shape: skills must be an array");
  process.exit();
}

const generatedAt = data.generated_at || "";
let ageHours = null;
if (generatedAt) {
  const then = Date.parse(generatedAt);
  if (Number.isFinite(then)) {
    ageHours = (Date.now() - then) / 3_600_000;
  }
}

const bySource = new Map();
for (const skill of skills) {
  const source = String(skill?.source || "");
  bySource.set(source, (bySource.get(source) || 0) + 1);
}

const issues = [];
if (ageHours === null) {
  issues.push("generated_at is missing or invalid");
} else if (ageHours > maxAgeHours) {
  issues.push(`index is ${ageHours.toFixed(1)}h old (limit ${maxAgeHours}h)`);
}
if (skills.length < minTotal) {
  issues.push(`total skills ${skills.length} < ${minTotal}`);
}
for (const [source, floor] of Object.entries(floors)) {
  const count =
    source === "skills.sh"
      ? (bySource.get("skills.sh") || 0) + (bySource.get("skills-sh") || 0)
      : bySource.get(source) || 0;
  if (count < floor) {
    issues.push(`${source}: ${count} < ${floor}`);
  }
}

if (issues.length) {
  fail(issues.join("; "));
  process.exit();
}

const summary = [...bySource.entries()]
  .sort((a, b) => b[1] - a[1])
  .slice(0, 8)
  .map(([source, count]) => `${source}=${count}`)
  .join(", ");
console.log(
  `[skills-index] ok: ${skills.length} skills, generated ${generatedAt}, ${summary}`,
);
