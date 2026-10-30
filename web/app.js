'use strict';
const $ = id => document.getElementById(id);
let key = sessionStorage.getItem('meshcore-web-key') || '';
let state = null, current = 'public', sending = false, polling = false, online = false, attempt = null, signature = '';
const labels = {queued:'Queued on Pi',submitted:'Accepted by radio · delivery unconfirmed',awaiting_ack:'Waiting for delivery acknowledgment',delivered:'Delivered',unconfirmed:'No acknowledgment received',unknown:'Send outcome unknown · check before retrying',failed:'Radio rejected message',received:'Received'};
function elem(tag, cls, text) {const el=document.createElement(tag); if(cls) el.className=cls; if(text!==undefined) el.textContent=text; return el;}
function matches(prefix) {return (state?.contacts||[]).filter(c=>c.prefix===prefix);}
function isChannel(target) {return target==='public'||target.startsWith('channel:');}
function channelTarget(c) {return c.index===state.public_channel?'public':`channel:${c.index}`;}
function channels() {return state?.channels?.length ? state.channels : state?.channels_ready ? [] : [{index:state?.public_channel||0,name:'Public'}];}
function name(target) {if(isChannel(target)) return channels().find(c=>channelTarget(c)===target)?.name || (target==='public'?'Public':`Channel ${target.slice(8)}`);const found=matches(target);return found.length===1 ? (found[0].name||target) : target;}
function maxBytes() {return isChannel(current) ? (state?.public_limit||120) : (state?.dm_limit||133);}
function canSend() {return online && state?.connected && ((isChannel(current)&&channels().some(c=>channelTarget(c)===current))||(!isChannel(current)&&!state.refreshing&&matches(current).length===1));}
async function api(path, options={}) {
  const response=await fetch(path,{...options,headers:{Authorization:`Bearer ${key}`,...options.headers},cache:'no-store'});
  const value=await response.json();
  if(!response.ok) {const error=new Error(value.error||`Request failed (${response.status})`);error.status=response.status;throw error;}
  return value;
}
function lock() {key='';state=null;online=false;sessionStorage.removeItem('meshcore-web-key');$('app').hidden=true;$('login').hidden=false;$('access-key').value='';$('messages').replaceChildren();$('message-input').value='';attempt=null;signature='';}
function choose(target) {current=target;signature='';$('message-input').value='';$('send-error').textContent='';attempt=null;render();$('message-input').focus();}
function nav(target, detail) {
  const button=elem('button',`conversation${current===target?' selected':''}`);button.type='button';button.setAttribute('aria-current',current===target?'true':'false');
  button.append(elem('span','avatar',isChannel(target)?'#':name(target).slice(0,2).toUpperCase()));
  const body=elem('span','label',name(target));body.append(elem('small','',detail));button.append(body);button.onclick=()=>choose(target);return button;
}
function updateComposer() {
  const bytes=new TextEncoder().encode($('message-input').value).length;
  $('byte-count').textContent=`${bytes} / ${maxBytes()} bytes`;
  $('send').disabled=sending||!canSend()||!$('message-input').value.trim()||bytes>maxBytes();
  $('message-input').disabled=!canSend();
  $('send').firstChild.textContent=sending?'Queuing… ':'Send message ';
}
function render() {
  if(!state)return;
  $('node-name').textContent=state.name;$('connection').textContent=online&&state.connected?'Radio connected':'Radio unavailable';$('status-dot').classList.toggle('online',online&&state.connected);
  const channelTargets = new Map(channels().map(c=>[channelTarget(c),`Channel ${c.index}`]));
  for(const m of state.messages)if(isChannel(m.conversation)&&!channelTargets.has(m.conversation))channelTargets.set(m.conversation,'Channel unavailable');
  $('channel-list').replaceChildren(...[...channelTargets].map(([target,detail])=>nav(target,detail)));
  $('channel-status').textContent=state.channel_status||'';
  $('join-channel').disabled=!online||!state.connected||state.channel_busy;
  $('refresh-channels').disabled=!online||state.channel_busy;
  const targets=new Set(state.contacts.map(c=>c.prefix));for(const m of state.messages)if(!isChannel(m.conversation))targets.add(m.conversation);
  $('dm-list').replaceChildren(...[...targets].sort((a,b)=>name(a).localeCompare(name(b))).map(t=>nav(t,matches(t).length===1?'Direct conversation':'Contact unavailable')));
  $('contacts-note').textContent=state.contacts_error||(!targets.size?'No contacts yet. Add a contact on your radio, then refresh.':'');
  $('refresh').disabled=state.refreshing;$('refresh').textContent=state.refreshing?'…':'↻';
  $('conversation-type').textContent=isChannel(current)?'CHANNEL':'DIRECT MESSAGE';$('conversation-title').textContent=name(current);
  $('conversation-subtitle').textContent=isChannel(current)?'A conversation with everyone listening.':`Contact · ${current}`;
  $('compose-label').textContent=`Message ${name(current)}`;
  $('send-help').textContent=isChannel(current)?'Everyone with this channel’s key can read these messages. Hashtags are public.':'A delivery acknowledgment confirms receipt, not that it was read.';
  const notice=!online?'Connection to the Pi lost. Reconnecting…':!state.connected?'Waiting for the radio…':!isChannel(current)&&matches(current).length!==1?'Refresh contacts to resolve this sender before replying.':'';
  $('notice').hidden=!notice;$('notice').textContent=notice;
  const messages=state.messages.filter(m=>m.conversation===current);$('message-count').textContent=`${messages.length} message${messages.length===1?'':'s'}`;
  const next=JSON.stringify([current,messages,state.contacts]);
  if(next!==signature) {
    const list=$('messages');const nearBottom=list.scrollHeight-list.scrollTop-list.clientHeight<100||!signature;
    const rows=messages.map(m=>{
      const row=elem('article',`message ${m.direction}`);const who=m.direction==='out'?'You':isChannel(current)?m.sender:name(current);
      row.append(elem('div','avatar',who.slice(0,2).toUpperCase()));const body=elem('div','message-body');const meta=elem('div','message-meta');
      const time=elem('time','',m.timestamp?new Date(m.timestamp*1000).toLocaleTimeString([],{hour:'numeric',minute:'2-digit'}):'Time unknown');if(m.timestamp)time.dateTime=new Date(m.timestamp*1000).toISOString();
      meta.append(elem('strong','',who),time);body.append(meta,elem('div','bubble',m.text));
      const foot=elem('div','message-bottom');foot.append(elem('span','',m.direction==='out'?(labels[m.status]||m.status):(m.snr_db===null?'Received':`${m.snr_db} dB · Received`)));
      if(m.direction==='in'){const reply=elem('button','reply','Reply ↗');reply.type='button';reply.onclick=()=>{if(isChannel(current))$('message-input').value=`@${who} `;$('message-input').focus();updateComposer();};foot.append(reply);}
      body.append(foot);row.append(body);return row;
    });
    if(!rows.length){const empty=elem('div','empty');empty.append(elem('div','empty-symbol','≈'),elem('strong','',isChannel(current)?'The channel is open.':'Start a conversation.'),elem('span','',isChannel(current)?'Messages heard by your node will appear here. Send a hello to get things started.':'Write a direct message below. Replies will appear in this conversation.'));rows.push(empty);}
    list.replaceChildren(...rows);if(nearBottom)list.scrollTop=list.scrollHeight;signature=next;
  }
  updateComposer();
}
async function poll() {
  if(!key||polling)return;polling=true;
  try {state=await api('/api/state');online=true;sessionStorage.setItem('meshcore-web-key',key);$('login').hidden=true;$('app').hidden=false;$('login-error').textContent='';render();}
  catch(e){online=false;if(e.status===401){lock();$('login-error').textContent='Access key not recognized.';}else if(state){render();}else{$('login-error').textContent='Cannot reach the Pi. Check the connection and try again.';}}
  finally{polling=false;}
}
$('login-form').onsubmit=async e=>{e.preventDefault();key=$('access-key').value.trim();await poll();};
$('logout').onclick=lock;
$('refresh').onclick=async()=>{try{await api('/api/contacts/refresh',{method:'POST'});await poll();}catch(e){$('send-error').textContent=e.message;}};
$('channel-form').onsubmit=async e=>{
  e.preventDefault();
  try {await api('/api/channels/join',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({name:$('hashtag').value})});await poll();}
  catch(e){$('channel-status').textContent=e.message;}
};
$('refresh-channels').onclick=async()=>{try{await api('/api/channels/refresh',{method:'POST'});await poll();}catch(e){$('channel-status').textContent=e.message;}};
$('message-input').oninput=updateComposer;
$('composer').onsubmit=async e=>{
  e.preventDefault();if(sending||!canSend())return;
  const text=$('message-input').value;if(!text.trim()||new TextEncoder().encode(text).length>maxBytes())return;
  if(!attempt||attempt.target!==current||attempt.text!==text){const bytes=crypto.getRandomValues(new Uint8Array(16));attempt={request_id:Array.from(bytes,b=>b.toString(16).padStart(2,'0')).join(''),target:current,text};}
  sending=true;updateComposer();$('send-error').textContent='';
  try{await api('/api/send',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(attempt)});$('message-input').value='';attempt=null;await poll();}
  catch(e){$('send-error').textContent=e.message||'Could not queue the message. Retrying this draft uses the same request ID.';}
  finally{sending=false;updateComposer();}
};
setInterval(poll,2000);poll();
