# toxi-template

Server-side templates with Jinja-style syntax. Templates load once;
call `reload()` to pick up disk changes.

```toml
[dependencies]
toxi-template = "3"
```

```rust
use toxi_template::{Context, TemplateContext};

let templates = TemplateContext::new("templates");
let mut ctx = Context::new();
ctx.set("name", "World");
let html = templates.render("hello.html", &ctx)?;
```
