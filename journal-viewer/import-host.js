// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// This function is installed by the viewer only in the journal's top document.
(contract, auth, nonce, origin) => {
  if (window !== window.top || location.origin !== origin) return;
  const native = window.webkit.messageHandlers.journalImport;
  const documentId = crypto.randomUUID();
  const sendPacket = async message => JSON.parse(await native.postMessage(JSON.stringify({nonce, documentId, message})));
  const attached = sendPacket({type:'host.attach'});
  const post = async message => { await attached; return sendPacket(message); };
  const authTypes = auth && auth.version === contract.version && auth.capability === contract.capability
    ? [auth.prepare, auth.authorize, auth.cancel]
    : [];
  const items = new Map();
  let timer;
  async function update() {
    for (const [item, record] of items) {
      try {
        const status = await post({type:'host.status', id:record.id, item});
        const serial = JSON.stringify(status);
        if (record.last !== serial) {
          record.last = serial;
          window.dispatchEvent(new CustomEvent(contract.event, {detail:{
            ...status, type:contract.status, item, name:record.name, id:record.id,
          }}));
        }
        if (['attention', 'delivered', 'gone'].includes(status.state)) items.delete(item);
      } catch (_) { /* Retry the status read while the document is open. */ }
    }
    timer = items.size ? setTimeout(update, 400) : undefined;
  }
  Object.defineProperty(window, contract.capability, {value:Object.freeze({
    version:contract.version, types:Object.freeze([contract.pick, ...authTypes]),
    async send(message) {
      if (!message || typeof message.id !== 'string' || typeof message.type !== 'string') throw new TypeError();
      if (authTypes.includes(message.type)) return await post(message);
      if (message.type !== contract.pick) return {type:contract.unsupported, id:message.id};
      const reply = await post(message);
      for (const item of reply.items || []) items.set(item.item, {id:message.id, name:item.name});
      if (items.size && !timer) timer = setTimeout(update, 0);
      return reply;
    }
  })});
  window.addEventListener(contract.drop, event => {
    // Cards can attach their own choices. A drop elsewhere is general intake.
    queueMicrotask(() => {
      if (!event.defaultPrevented && event.detail?.token)
        window[contract.capability].send({type:contract.pick,id:crypto.randomUUID(),area:'quick',
          accept:[],multiple:true,context:{},drop:event.detail.token}).catch(() => {});
    });
  });
  const ready = () => {
    if (window.solstoneImportContract?.version === contract.version)
      post({type:'host.ready'}).catch(() => {});
  };
  document.addEventListener('DOMContentLoaded', ready, {once:true});
}
