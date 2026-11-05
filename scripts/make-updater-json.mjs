#!/usr/bin/env node
/* 生成 updater.json —— 应用检查更新时读的就是这份清单。
 *
 *   用法：node scripts/make-updater-json.mjs --version 1.6.0
 *         （安装包默认取 dist/DeskList-Setup-<version>.exe，可用 --file 指定）
 *
 * 生成后两条路都能让客户端看见：
 *   1) 作为 Release 附件上传（首选，releases/latest 永远指向最新版，无缓存）
 *      gh release upload v1.6.0 updater.json
 *   2) 提交进仓库，客户端会用 jsDelivr 兜底读取（有缓存延迟，只做备份）
 */
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";

const argv = process.argv.slice(2);
const arg = (k, d) => {
  const i = argv.indexOf("--" + k);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : d;
};

const version = String(arg("version", "")).replace(/^v/i, "");
if (!/^\d+(\.\d+){0,3}$/.test(version)) {
  console.error("用法：node scripts/make-updater-json.mjs --version 1.6.0 [--file dist/DeskList-Setup-1.6.0.exe] [--repo WoBok/desklist]");
  process.exit(1);
}

const repo = arg("repo", "WoBok/desklist");
const file = arg("file", path.join("dist", `DeskList-Setup-${version}.exe`));
if (!fs.existsSync(file)) {
  console.error("找不到安装包：" + file);
  process.exit(1);
}

const buf = fs.readFileSync(file);
const manifest = {
  version,
  url: `https://github.com/${repo}/releases/download/v${version}/${path.basename(file)}`,
  sha256: crypto.createHash("sha256").update(buf).digest("hex"),
  size: buf.length,
  pub_date: new Date().toISOString()
};

const out = path.join(process.cwd(), "updater.json");
fs.writeFileSync(out, JSON.stringify(manifest, null, 2) + "\n", "utf8");

console.log(`已写入 ${out}`);
console.log(`  版本    ${manifest.version}`);
console.log(`  大小    ${(manifest.size / 1024 / 1024).toFixed(2)} MB`);
console.log(`  sha256  ${manifest.sha256}`);
console.log(`  地址    ${manifest.url}`);
