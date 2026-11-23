#!/usr/bin/env node
// Exercise the shipped page controller with a fake gateway and a minimal DOM.
// This never starts a browser, sends SMS, or contacts a real account.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const root = new URL('../', import.meta.url);
const html = fs.readFileSync(new URL('ui/lingxi/index.html', root), 'utf8');
const source = fs.readFileSync(new URL('ui/lingxi/main.js', root), 'utf8');
assert.ok(source.startsWith("import { request } from './api.js';"));
const nodes = new Map();
for (const match of html.matchAll(/<([a-z-]+)\b([^>]*\bid="([^"]+)"[^>]*)>/g)) {
  const [, tag, attrs, id] = match;
  nodes.set(id, {
    id, tag, value: '', textContent: '', hidden: /\bhidden\b/.test(attrs),
    disabled: /\bdisabled\b/.test(attrs), listeners: {},
    addEventListener(name, fn) { this.listeners[name] = fn; },
    setAttribute(name, value) { this[name] = value; },
    removeAttribute(name) { delete this[name]; },
    checkValidity() { return true; }, reportValidity() {},
    querySelectorAll() { return [...nodes.values()].filter(n => n.tag === 'input'); },
  });
}
const node = id => { assert.ok(nodes.has(id), id); return nodes.get(id); };
node('phone').value = '13800138000';
node('proxy-mode').value = 'direct';
let status = {status:'ready',message:'请发送短信验证码'};
const captcha = {status:'captcha',message:'请输入图片中的验证码',captcha_image:'data:image/png;base64,AA=='};
let waitHook, verifySuccess = false, sendFailure = false, nextTimer = 0;
const timers = new Map();
const context = vm.createContext({
  URL, Date, Error, JSON, document: {getElementById:node},
  window: {addEventListener() {}},
  setTimeout(fn) { timers.set(++nextTimer, fn); return nextTimer; },
  clearTimeout(id) { timers.delete(id); }, setInterval() { return 1; }, clearInterval() {},
  async request(path, options = {}) {
    if (path.endsWith('/start')) return {state:'fixture-flow',instructions:JSON.stringify(status)};
    if (path.includes('/wait?')) {
      if (waitHook) { const hook = waitHook; waitHook = null; return hook(); }
      return {pending:true,hint:JSON.stringify(status)};
    }
    if (path.endsWith('/callback')) {
      const command = JSON.parse(new URL(options.body.callbackUrl).searchParams.get('code'));
      if (command.action === 'captcha') status = captcha;
      if (command.action === 'send') {
        assert.equal(command.captcha_code, 'ABCD');
        status = sendFailure ? {status:'error',message:'发送失败'} : {status:'sent',message:'短信已发送，请输入 6 位验证码'};
      }
      if (command.action === 'verify') {
        if (verifySuccess) return {accountId:'fixture-account'};
        status = {status:'invalid_code',message:'短信验证码错误或已过期，请重新输入'};
      }
      return {accepted:true};
    }
    if (path.endsWith('/cancel')) return {canceled:true};
    throw new Error('Unexpected fixture request: ' + path);
  },
});
vm.runInContext(source.replace("import { request } from './api.js';", ''), context);
assert.equal(node('verify').disabled, true);
await context.run('captcha');
assert.equal(node('captcha-panel').hidden, false);
node('captcha-code').value = 'ABCD';
await context.poll('fixture-flow');
assert.equal(node('captcha-code').value, 'ABCD');
console.log('PASS repeated captcha poll preserves typed input');

let finishOldPoll;
waitHook = () => new Promise(resolve => { finishOldPoll = resolve; });
const oldPoll = context.poll('fixture-flow');
await context.run('send');
assert.equal(node('verify').disabled, false);
finishOldPoll({pending:true,hint:JSON.stringify({...captcha,message:'stale captcha'})});
await oldPoll;
assert.equal(node('status').textContent, '短信已发送，请输入 6 位验证码');
assert.equal(node('captcha-code').value, 'ABCD');
assert.equal(node('verify').disabled, false);
console.log('PASS stale poll cannot overwrite successful send');

node('sms-code').value = '000000';
await context.run('verify');
assert.equal(node('error').hidden, false);
assert.equal(node('verify').disabled, false);
assert.equal(node('sms-code').value, '000000');
console.log('PASS wrong SMS code remains correctable');
verifySuccess = true;
node('sms-code').value = '123456';
await context.run('verify');
assert.match(node('status').textContent, /账户已保存/);
assert.equal(node('sms-code').value, '');
assert.equal(node('verify').disabled, true);
assert.equal(node('cancel').disabled, true);
assert.equal(node('error').hidden, true);
console.log('PASS confirmed account save clears login flow');

status = {status:'ready',message:'请发送短信验证码'};
await context.run('captcha');
node('captcha-code').value = 'ABCD'; sendFailure = true;
await context.run('send');
assert.equal(node('verify').disabled, true);
assert.equal(node('error').textContent, '发送失败');
await context.cancel();
assert.equal(node('cancel').disabled, true);
console.log('PASS failed send cannot enable login and can be cancelled');
console.log('5 UI regression checks passed; no browser or live SMS used.');
