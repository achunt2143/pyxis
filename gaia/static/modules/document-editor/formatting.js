/**
 * Syntax Highlight Component
 * File: gaia/static/modules/document-editor/syntax-highlight.js
 * Purpose: Code syntax highlighting
 */
class SyntaxHighlighter {
  constructor() {
    this.keywords = ['function', 'const', 'let', 'var', 'if', 'else', 'for', 'while', 'return'];
    this.patterns = {
      comment: /\/\/.*$/gm,
      string: /(['"`]).*?\1/g,
      number: /\b\d+\b/g,
      keyword: new RegExp(`\\b(${this.keywords.join('|')})\\b`, 'g'),
    };
  }

  highlight(code) {
    let highlighted = code;

    Object.entries(this.patterns).forEach(([type, pattern]) => {
      highlighted = highlighted.replace(pattern, (match) => {
        return `<span class="hljs-${type}">${match}</span>`;
      });
    });

    return highlighted;
  }

  apply(element) {
    const code = element.textContent;
    element.innerHTML = this.highlight(code);
  }
}

export default SyntaxHighlighter;