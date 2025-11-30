/**
 * Terminal Component
 * File: gaia/static/modules/terminal/terminal.js
 */

import { ModuleLoader } from '../../scripts/module-loader.js';

class Terminal {
  constructor(options) {
    this.element = options.element;
    this.output = [];
  }

  async initialize() {
    await ModuleLoader.loadStyles('terminal');
    this.createStructure();
  }

  createStructure() {
    this.element.innerHTML = `
      <div class="terminal-container">
        <div class="terminal-output" id="terminal-output"></div>
        <input type="text" class="terminal-input" id="terminal-input" placeholder="$ ">
      </div>
    `;

    const input = document.getElementById('terminal-input');
    input.addEventListener('keypress', (e) => this.handleCommand(e));
  }

  handleCommand(event) {
    if (event.key !== 'Enter') return;

    const command = event.target.value;
    this.executeCommand(command);
    event.target.value = '';
  }

  executeCommand(command) {
    const output = document.getElementById('terminal-output');
    output.innerHTML += `<div>$ ${command}</div>`;
  }

  async destroy() {}
}
export default Terminal;