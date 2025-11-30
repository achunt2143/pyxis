/**
 * Formatting Component
 * File: gaia/static/modules/document-editor/formatting.js
 * Purpose: Text formatting utilities
 */
class TextFormatter {
  static formatBold(text) {
    return `<strong>${text}</strong>`;
  }

  static formatItalic(text) {
    return `<em>${text}</em>`;
  }

  static formatUnderline(text) {
    return `<u>${text}</u>`;
  }

  static formatHeading(text, level = 1) {
    return `<h${level}>${text}</h${level}>`;
  }

  static formatList(items) {
    const listItems = items.map(item => `<li>${item}</li>`).join('');
    return `<ul>${listItems}</ul>`;
  }

  static formatCode(text) {
    return `<code>${text}</code>`;
  }
}

export default TextFormatter;