/**
 * GeckoOS Extension - Content Script (FIXED)
 * 
 * Runs on every web page and injects the gaia-bridge.js script
 * into the page context so it can access window object.
 */

console.log('[GeckoOS Extension] Content script loaded');

// Inject the gaia-bridge.js script into the page context
function injectBridgeScript() {
  try {
    // Create script element
    const script = document.createElement('script');
    script.src = browser.runtime.getURL('gaia-bridge.js');
    script.type = 'text/javascript';

    // Inject as soon as possible
    if (document.documentElement) {
      document.documentElement.insertBefore(script, document.documentElement.firstChild);
    } else {
      document.head.appendChild(script);
    }

    console.log('[GeckoOS Extension] gaia-bridge.js injected successfully');

  } catch (error) {
    console.error('[GeckoOS Extension] Failed to inject gaia-bridge.js:', error);
  }
}

// Inject as early as possible
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', injectBridgeScript);
} else {
  injectBridgeScript();
}

/**
 * FIXED: Listen for messages from the injected script (via window.postMessage)
 * These messages come from gaia-bridge.js in the page context
 */
window.addEventListener('message', (event) => {
  if (event.source !== window) return;

  const data = event.data;
  console.log('[GeckoOS Extension] [PAGE->EXT] Received from page:', data.type);

  // Handle bridge initialization requests
  if (data.type === 'GECKOOSPBRIDGE_INIT_REQUEST') {
    console.log('[GeckoOS Extension] [PAGE->EXT] Bridge initialization request received');
    
    // Send initialization response back to page
    window.postMessage({
      type: 'GECKOOSPBRIDGE_INIT_RESPONSE',
      source: 'extension'
    }, '*');
    return;
  }

  // Handle service calls from Gaia web app
  if (data.type === 'GECKOOSPBRIDGE_CALL') {
    console.log(`[GeckoOS Extension] [PAGE->EXT] Service call: ${data.service}.${data.method} (requestId: ${data.requestId})`);

    // Forward to background script which handles native messaging
    browser.runtime.sendMessage({
      type: 'ECHO_SERVICE_CALL',
      requestId: data.requestId,  // ← Keep original ID
      service: data.service,
      method: data.method,
      params: data.params,
      timestamp: data.timestamp
    }, (response) => {
      // FIXED: Handle callback response (but don't rely on it for final response)
      if (browser.runtime.lastError) {
        console.error('[GeckoOS Extension] [PAGE->EXT] Error communicating with background:', browser.runtime.lastError);
        
        // Send error response back to page
        window.postMessage({
          type: 'GECKOOSPBRIDGE_RESPONSE',
          requestId: data.requestId,
          error: browser.runtime.lastError.message
        }, '*');
      }
      // Note: We don't send the response here anymore.
      // The actual response will come via browser.runtime.onMessage listener below.
    });
  }
});

/**
 * FIXED: Listen for responses from background script (via browser.runtime.sendMessage)
 * This is the proper way to receive messages from background to content script
 */
browser.runtime.onMessage.addListener((message, sender) => {
  console.log('[GeckoOS Extension] [BG->EXT] Received from background:', message.type);

  // Handle service responses
  if (message.type === 'GECKOOSPBRIDGE_RESPONSE') {
    console.log(`[GeckoOS Extension] [BG->EXT] Service response for requestId: ${message.requestId}`);
    
    // Forward response back to page via postMessage
    window.postMessage({
      type: 'GECKOOSPBRIDGE_RESPONSE',
      requestId: message.requestId,
      result: message.result || null,
      error: message.error || null
    }, '*');
  }

  // Handle service events/signals
  if (message.type === 'GECKOOSPBRIDGE_EVENT') {
    console.log(`[GeckoOS Extension] [BG->EXT] Event from ${message.service}: ${message.event}`);
    
    // Forward event to page
    window.postMessage({
      type: 'GECKOOSPBRIDGE_EVENT',
      service: message.service,
      event: message.event,
      data: message.data
    }, '*');
  }

  // Handle status updates
  if (message.type === 'GECKOOSPBRIDGE_STATUS') {
    console.log(`[GeckoOS Extension] [BG->EXT] Status: ${message.status}`);
    
    // Forward status to page
    window.postMessage({
      type: 'GECKOOSPBRIDGE_STATUS',
      status: message.status,
      details: message.details
    }, '*');
  }
});

console.log('[GeckoOS Extension] Content script initialized');
