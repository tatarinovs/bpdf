// Added by bpdf to an HTML page before printing it to PDF; `__RATIO__` is the
// height / width of the target paper. Sizes the paper to the content, unless
// the page chooses its own with `@page { size }`. Must not contain a closing
// script tag.
(function () {
  var RATIO = __RATIO__;

  function hasPageSize() {
    for (var i = 0; i < document.styleSheets.length; i++) {
      var rules;
      try {
        rules = document.styleSheets[i].cssRules;
      } catch (e) {
        continue; // cross-origin stylesheet, e.g. Google Fonts
      }
      for (var j = 0; j < rules.length; j++) {
        if (rules[j].type === 6 && rules[j].style.getPropertyValue("size")) return true;
      }
    }
    return false;
  }

  function sizePaper() {
    if (hasPageSize()) return;
    var root = document.documentElement;
    var body = document.body || root;
    var width = Math.max(root.scrollWidth, body.scrollWidth, 1);
    var height = Math.max(root.scrollHeight, body.scrollHeight, 1);
    // One sheet of exactly the content's size, or sheets of its width.
    var pageHeight = height > width * RATIO ? Math.round(width * RATIO) : height;
    var style = document.createElement("style");
    style.textContent = "@page{size:" + width + "px " + pageHeight + "px;margin:0}";
    (document.head || root).appendChild(style);
  }

  function start() {
    (document.fonts ? document.fonts.ready : Promise.resolve()).then(sizePaper, sizePaper);
  }

  if (document.readyState === "complete") start();
  else addEventListener("load", start);
})();
