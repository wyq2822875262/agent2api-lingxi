import { request } from './api.js';
const $ = id => document.getElementById(id);
let flow = '', busy = false, sent = false, manual = false, deadline = 0, resendAt = 0;
let timer, watch;
let lastHint = '', pollGeneration = 0;
function pausePolling() { clearTimeout(watch); pollGeneration++; }
const proxy = () => $('proxy-mode').value === 'direct' ? { proxy: { source: 'direct' } } : {};
function error(value = '') { $('error').hidden = !value; $('error').textContent = value instanceof Error ? value.message : String(value); }
function controls() {
  for (const id of ['send','captcha','sms-tab','manual-tab','import']) $(id).disabled = busy;
  $('send').disabled = busy || Date.now() < resendAt;
  $('send').textContent = Date.now() < resendAt ? `${Math.ceil((resendAt - Date.now()) / 1000)} 秒后重发` : '发送短信验证码';
  $('verify').disabled = busy || !sent || !flow;
  $('cancel').disabled = busy || !flow;
  $('phone').disabled = busy || Boolean(flow);
  $('proxy-mode').disabled = busy || Boolean(flow);
  for (const form of ['manual-form','sms-form']) for (const field of $(form).querySelectorAll('input')) field.disabled = busy || (field.id === 'phone' && Boolean(flow));
}
function clearFlow() {
  flow = ''; sent = false; deadline = 0; lastHint = ''; pausePolling();
  $('sms-code').value = ''; $('captcha-code').value = '';
  $('captcha-panel').hidden = true; $('captcha-image').removeAttribute('src'); controls();
}
function confirmed(state) {
  return state?.done === true && state.session && (!state.session.provider || state.session.provider === 'lingxi') && [state.session.accountUid,state.session.accountId].some(v => typeof v === 'string' && v);
}
function success() { clearFlow(); error(); $('status').textContent = '账户已保存。请回到账户列表刷新移动灵犀模型目录。'; }
function hint(value) {
  // Polls repeat the same captcha/status. Re-rendering it would erase the code
  // the user is entering and could overwrite a newer error or progress message.
  if (value === lastHint) return;
  lastHint = value;
  let data;
  try { data = JSON.parse(value); } catch { if (value) $('status').textContent = String(value); return; }
  if (typeof data.message === 'string') $('status').textContent = data.message;
  if (data.status === 'sent') { sent = true; error(); }
  if (data.status === 'captcha' && /^data:image\/(png|jpeg|gif|webp);base64,[a-z0-9+/=\s]+$/i.test(data.captcha_image || '')) {
    $('captcha-image').src = data.captcha_image; $('captcha-panel').hidden = false; $('captcha-code').value = '';
  }
  if (['captcha_required','invalid_code','rate_limited','error'].includes(data.status)) error(data.message || '请检查输入后重试');
  controls();
}
async function stateOf(id) { return request('/api/session/login/wait?state=' + encodeURIComponent(id)); }
async function poll(id) {
  if (busy || id !== flow) return;
  const generation = pollGeneration;
  try {
    const state = await stateOf(id);
    if (id !== flow || generation !== pollGeneration) return;
    if (confirmed(state)) { success(); return; }
    if (state?.done) { clearFlow(); error(state.error || '登录已结束，请重新开始'); return; }
    if (state?.hint) hint(state.hint);
    watch = setTimeout(() => void poll(id), 1800);
  } catch (e) { if (id === flow && generation === pollGeneration) { error(e); watch = setTimeout(() => void poll(id), 4000); } }
}
async function begin() {
  if (flow) return;
  if (!$('phone').checkValidity()) { $('phone').reportValidity(); throw new Error('请填写 11 位手机号'); }
  const result = await request('/api/session/login/start', { method: 'POST', body: { provider:'lingxi', methodIndex:1, inputs:{phone:$('phone').value.trim()}, ...proxy() } });
  const id = result?.state || result?.flow;
  if (typeof id !== 'string' || !id) throw new Error('网关没有返回登录会话，请检查插件状态');
  flow = id; deadline = Date.now() + 300000; lastHint = ''; hint(result.instructions || '');
  clearTimeout(watch); watch = setTimeout(() => void poll(id), 1000);
}
async function command(action) {
  await begin();
  clearTimeout(watch);
  const id = flow;
  const code = { action, captcha_code:$('captcha-code').value.trim() };
  if (action === 'verify') Object.assign(code, {sms_code:$('sms-code').value.trim(),name:$('sms-name').value.trim()});
  // This URL is passed as data to the gateway callback API; it is never opened or fetched.
  const callback = new URL('https://ai.soho.komect.com/agent2api-login');
  callback.searchParams.set('state',id); callback.searchParams.set('code',JSON.stringify(code));
  const result = await request('/api/session/login/callback', {method:'POST',body:{state:id,callbackUrl:callback.href}});
  if (id !== flow) return;
  if (result?.accountId) { success(); return; }
  const state = await stateOf(id);
  if (id !== flow) return;
  if (confirmed(state)) { success(); return; }
  if (state?.done) { clearFlow(); throw new Error(state.error || '登录已结束，请重新开始'); }
  if (state?.hint) hint(state.hint);
  else throw new Error('网关尚未返回登录状态，请稍后查看状态提示');
}
async function run(action) {
  if (busy) return;
  pausePolling();
  busy = true; error(); controls();
  try {
    if (action === 'send') resendAt = Date.now() + 60000;
    await command(action);
  } catch (e) { error(e); }
  finally {
    busy = false; controls();
    if (flow) { clearTimeout(watch); watch = setTimeout(() => void poll(flow), 1000); }
  }
}
async function cancel() {
  if (!flow) return false;
  pausePolling();
  const id = flow;
  const result = await request('/api/session/login/cancel',{method:'POST',body:{state:id}});
  if (result?.canceled === true) { clearFlow(); $('status').textContent = '本次登录已取消。'; return false; }
  const state = await stateOf(id);
  if (confirmed(state)) { success(); return true; }
  if (state?.done) {clearFlow(); return false;}
  throw new Error('尚未确认取消，请稍后重试');
}
$('send').addEventListener('click',() => void run('send'));
$('captcha').addEventListener('click',() => void run('captcha'));
$('sms-form').addEventListener('submit',event => {event.preventDefault(); void run('verify');});
$('cancel').addEventListener('click',async () => {if (busy) return;busy=true;controls();try {await cancel();}catch(e){error(e);}finally{busy=false;controls();}});
async function choose(next) {
  if (busy || manual === next) return;
  busy=true;controls();try {
    if (await cancel()) return;
    manual=next;error();$('sms-panel').hidden=manual;$('manual-panel').hidden=!manual;
    $('sms-tab').setAttribute('aria-selected',String(!manual));$('manual-tab').setAttribute('aria-selected',String(manual));
    $('status').textContent='登录后，回到账户列表刷新模型目录。';
  }catch(e){error(e);}finally{busy=false;controls();}
}
$('sms-tab').addEventListener('click',()=>void choose(false));$('manual-tab').addEventListener('click',()=>void choose(true));
$('manual-form').addEventListener('submit',async event => {
  event.preventDefault();if(busy)return;busy=true;error();controls();
  try {
    const result=await request('/api/accounts',{method:'POST',body:{provider:'lingxi',methodIndex:0,inputs:{access_token:$('access-token').value,refresh_token:$('refresh-token').value},name:$('manual-name').value,...proxy()}});
    if(!result?.account?.id || result.account.provider!=='lingxi')throw new Error('网关未确认账户已保存，请检查账户列表');
    $('access-token').value='';$('refresh-token').value='';success();
  }catch(e){error(e);}finally{busy=false;controls();}
});
timer=setInterval(()=>{controls();},1000);
window.addEventListener('pagehide',()=>{clearInterval(timer);clearTimeout(watch);if(flow)void request('/api/session/login/cancel',{method:'POST',body:{state:flow},keepalive:true}).catch(()=>{});});
controls();
