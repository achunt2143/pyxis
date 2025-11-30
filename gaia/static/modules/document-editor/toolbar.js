/**
 * Toolbar Component
 * File: gaia/static/modules/document-editor/toolbar.js
 * Purpose: Editor toolbar
 */
class EditorToolbar {
  constructor(options) {
    this.container = options.container;
    this.editor = options.editor;
  }

  render() {
    const html = `
      <div class="editor-toolbar-buttons">
        <button class="toolbar-btn" data-action="bold" title="Bold (Ctrl+B)">
          <strong>B</strong>
        </button>
        <button class="toolbar-btn" data-action="italic" title="Italic (Ctrl+I)">
          <i>I</i>
        </button>
        <button class="toolbar-btn" data-action="underline" title="Underline (Ctrl+U)">
          <u>U</u>
        </button>
        <span class="toolbar-divider"></span>
        <button class="toolbar-btn" data-action="undo" title="Undo">↶</button>
        <button class="toolbar-btn" data-action="redo" title="Redo">↷</button>
        <span class="toolbar-divider"></span>
        <button class="toolbar-btn" data-action="save" title="Save (Ctrl+S)">💾</button>
      </div>
    `;

    this.container.innerHTML = html;
    this.attachListeners();
  }

  attachListeners() {
    const buttons = this.container.querySelectorAll('.toolbar-btn');
    buttons.forEach(btn => {
      btn.addEventListener('click', () => {
        const action = btn.dataset.action;
        this.performAction(action);
      });
    });
  }

  performAction(action) {
    switch (action) {
      case 'bold':
        document.execCommand('bold', false, null);
        break;
      case 'italic':
        document.execCommand('italic', false, null);
        break;
      case 'underline':
        document.execCommand('underline', false, null);
        break;
      case 'undo':
        document.execCommand('undo', false, null);
        break;
      case 'redo':
        document.execCommand('redo', false, null);
        break;
      case 'save':
        window.dispatchEvent(new CustomEvent('editor:save'));
        break;
    }
  }
}

export default EditorToolbar;