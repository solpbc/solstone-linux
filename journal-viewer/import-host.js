// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// This function is installed by the viewer only in the journal's top document.
(contract, auth, exportContract, nonce, origin) => {
  if (window !== window.top || location.origin !== origin) return;
  const native = window.webkit.messageHandlers.journalImport;
  const documentId = crypto.randomUUID();
  const sendPacket = async message => JSON.parse(await native.postMessage(JSON.stringify({nonce, documentId, message})));
  const attached = sendPacket({type:'host.attach'});
  const post = async message => { await attached; return sendPacket(message); };
  const authTypes = auth && auth.version === contract.version && auth.capability === contract.capability
    ? [auth.prepare, auth.authorize, auth.cancel]
    : [];
  const exportTypes = exportContract && exportContract.version === contract.version && exportContract.capability === contract.capability
    ? [
        exportContract.start,
        exportContract.status,
        exportContract.pause,
        exportContract.resume,
        exportContract.cancel,
        exportContract.reveal,
        exportContract.deviceSpace,
      ]
    : [];
  const items = new Map();
  let timer;
  let updating = false;
  let pendingNew = false;
  let exportTimer;
  let exportLastSerial;
  async function pollExport() {
    exportTimer = undefined;
    if (!exportContract) return;
    try {
      const status = await post({type:exportContract.status, id:crypto.randomUUID()});
      const serial = JSON.stringify(status);
      if (exportLastSerial !== serial) {
        exportLastSerial = serial;
        window.dispatchEvent(new CustomEvent(exportContract.event, {detail: status}));
      }
      if (status && ['picking', 'transferring', 'verifying'].includes(status.phase)) {
        exportTimer = setTimeout(pollExport, 250);
      }
    } catch (_) {
      exportTimer = setTimeout(pollExport, 600);
    }
  }
  function trackExportReply(reply) {
    if (!exportContract || !reply || typeof reply !== 'object') return;
    exportLastSerial = JSON.stringify(reply);
    if (['picking', 'transferring', 'verifying'].includes(reply.phase)) {
      if (!exportTimer) exportTimer = setTimeout(pollExport, 250);
    } else if (exportTimer) {
      clearTimeout(exportTimer);
      exportTimer = undefined;
    }
  }
  async function update() {
    updating = true;
    timer = undefined;
    pendingNew = false;
    let nextDelay;
    try {
      for (const [item, record] of [...items]) {
        try {
          const status = await post({type:'host.status', id:record.id, item});
          const serial = JSON.stringify(status);
          if (record.last !== serial) {
            record.last = serial;
            window.dispatchEvent(new CustomEvent(contract.event, {detail:{
              ...status, type:contract.status, item, name:record.name, id:record.id,
            }}));
          }
          const keepPollingReview = status.state === 'attention' && status.review_required === true;
          if (status.state === 'delivered' || status.state === 'gone'
              || (status.state === 'attention' && !keepPollingReview)) {
            items.delete(item);
          } else {
            const delay = keepPollingReview ? 2000 : 400;
            nextDelay = nextDelay === undefined ? delay : Math.min(nextDelay, delay);
          }
        } catch (_) {
          nextDelay = nextDelay === undefined ? 400 : Math.min(nextDelay, 400);
        }
      }
    } finally {
      updating = false;
      timer = items.size ? setTimeout(update, pendingNew ? 0 : (nextDelay ?? 400)) : undefined;
    }
  }
  Object.defineProperty(window, contract.capability, {value:Object.freeze({
    version:contract.version, types:Object.freeze([contract.pick, ...authTypes, ...exportTypes]),
    async send(message) {
      if (!message || typeof message.id !== 'string' || typeof message.type !== 'string') throw new TypeError();
      if (authTypes.includes(message.type)) return await post(message);
      if (exportTypes.includes(message.type)) {
        const reply = await post(message);
        if (message.type !== exportContract.deviceSpace && message.type !== exportContract.reveal) {
          trackExportReply(reply);
        }
        return reply;
      }
      if (message.type !== contract.pick) return {type:contract.unsupported, id:message.id};
      const reply = await post(message);
      for (const item of reply.items || []) {
        items.set(item.item, {id:message.id, name:item.name});
        pendingNew = true;
      }
      if (items.size && !updating) {
        clearTimeout(timer);
        timer = setTimeout(update, 0);
      }
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
