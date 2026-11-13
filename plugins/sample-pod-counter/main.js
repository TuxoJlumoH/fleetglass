let reqId = 0;
const pending = new Map();

window.addEventListener("message", function(event) {
  const data = event.data;
  if (data && data.type === "fleetglass:response" && data.id) {
    const resolve = pending.get(data.id);
    if (resolve) {
      pending.delete(data.id);
      resolve(data);
    }
  }
});

function request(action, payload) {
  return new Promise(function(resolve) {
    const id = String(++reqId);
    pending.set(id, resolve);
    window.parent.postMessage({
      type: "fleetglass:request",
      id: id,
      action: action,
      payload: payload
    }, "*");
  });
}

async function loadData() {
  const statusEl = document.getElementById("status");
  const countEl = document.getElementById("pod-count");
  const ctxEl = document.getElementById("context-info");

  statusEl.textContent = "Querying data via bridge...";
  try {
    const ctxRes = await request("read:contexts");
    const resRes = await request("read:resources");

    if (ctxRes.error) {
      statusEl.textContent = "Contexts: " + ctxRes.error;
      return;
    }
    if (resRes.error) {
      statusEl.textContent = "Resources: " + resRes.error;
      return;
    }

    const contexts = Array.isArray(ctxRes.data) ? ctxRes.data : [];
    const resources = Array.isArray(resRes.data) ? resRes.data : [];

    ctxEl.textContent = "Connected contexts: " + contexts.length;
    countEl.textContent = resources.length + " Pods";
    statusEl.textContent = "Updated successfully";
  } catch (err) {
    statusEl.textContent = "Error: " + (err && err.message ? err.message : String(err));
  }
}

document.getElementById("refresh-btn").addEventListener("click", loadData);
loadData();
