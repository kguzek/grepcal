use axum::response::Html;

const SERVER_REVISION: &str = env!("GREPCAL_REVISION");
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY_URL: &str = "https://git.guzek.uk/kguzek/grepcal";
const FORGEJO_LOGO_URL: &str = "https://git.guzek.uk/assets/img/logo.svg";

pub async fn home() -> Html<String> {
    Html(format!(
        "<!doctype html>\
         <html lang=\"en\">\
         <head>\
         <meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <meta name=\"theme-color\" content=\"#0a2433\">\
         <title>grepcal</title>\
         <style>\
         :root {{ color: #e6f7f8; background: #080d12; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; }}\
         * {{ box-sizing: border-box; }}\
         body {{ width: 100vw; height: 100vh; margin: 0; overflow: hidden; background: radial-gradient(circle at top right, #0a3546, transparent 38rem), #080d12; }}\
         main {{ position: fixed; top: 50%; left: 50%; width: min(calc(100vw - 3rem), 44rem); max-height: calc(100vh - 12rem); overflow: auto; padding: clamp(2rem, 8vw, 5rem); border: 1px solid #1e5162; border-radius: 1rem; background: linear-gradient(145deg, #0d1e29, #091218); box-shadow: 0 1.5rem 5rem #0008; transform: translate(-50%, -50%); }}\
         .eyebrow {{ margin: 0 0 1rem; color: #5de1e6; font-size: .75rem; font-weight: 700; letter-spacing: .14em; text-transform: uppercase; }}\
         h1 {{ margin: 0; color: #fff; font-size: clamp(3rem, 12vw, 6rem); line-height: .85; letter-spacing: -.08em; }}\
         .subtitle {{ max-width: 32rem; margin: 1.75rem 0 2.5rem; color: #b9d6da; font-size: clamp(1.125rem, 2vw, 1.4rem); line-height: 1.5; }}\
         .cta {{ display: inline-block; padding: .85rem 1.1rem; border-radius: .5rem; color: #031316; background: #5de1e6; font-weight: 700; text-decoration: none; }}\
         .cta:hover {{ background: #a0f5f4; }}\
         .repo {{ display: block; margin-top: 3.5rem; padding: 1.25rem; border: 1px solid #1e5162; border-radius: .75rem; color: inherit; background: #07151d; text-decoration: none; }}\
         .repo:hover {{ border-color: #5de1e6; }}\
         .repo-heading {{ display: flex; align-items: center; gap: .75rem; margin-bottom: .5rem; }}\
         .repo img {{ width: 1.75rem; height: 1.75rem; }}\
         .repo-label {{ color: #7babb3; font-size: .75rem; letter-spacing: .1em; text-transform: uppercase; }}\
         .repo-url {{ display: block; color: #5de1e6; font-size: .95rem; overflow-wrap: anywhere; }}\
         footer {{ position: fixed; bottom: 1.5rem; left: 0; display: flex; flex-direction: column; align-items: center; gap: .5rem; width: 100%; padding: 0 1.5rem; color: #6e9298; font-size: .75rem; line-height: 1.5; text-align: center; }}\
         footer p {{ margin: 0; }}\
         footer a {{ color: inherit; transition: color 300ms ease; }}\
         footer a:hover {{ color: #fff; }}\
         @media (min-width: 48rem) {{ footer {{ flex-direction: row; justify-content: center; gap: 1.5rem; }} }}\
         code {{ color: #b9d6da; }}\
         </style>\
         </head>\
         <body>\
         <main>\
         <p class=\"eyebrow\">Calendar filtering service</p>\
         <h1>grepcal</h1>\
         <p class=\"subtitle\">Like grep, but for iCal!</p>\
         <a class=\"cta\" href=\"{REPOSITORY_URL}?utm_source=grepcal&amp;utm_medium=referral&amp;utm_content=self_host\">Self-host it – free and open source</a>\
         <a class=\"repo\" href=\"{REPOSITORY_URL}?utm_source=grepcal&amp;utm_medium=referral&amp;utm_content=source_repository\">\
         <span class=\"repo-heading\">\
         <img src=\"{FORGEJO_LOGO_URL}\" alt=\"Forgejo\">\
         <span class=\"repo-label\">Source repository</span>\
         </span>\
         <span class=\"repo-url\">git.guzek.uk/kguzek/grepcal</span>\
         </a>\
         </main>\
         <footer>\
         <p>Copyright © 2026 <a href=\"https://www.guzek.uk?utm_source=grepcal&amp;utm_medium=referral&amp;utm_content=footer\">Konrad Guzek</a></p>\
         <p>Server revision: <code>v{SERVER_VERSION} ({SERVER_REVISION})</code></p>\
         </footer>\
         </body>\
         </html>"
    ))
}

#[cfg(test)]
mod tests {
    use super::{home, REPOSITORY_URL, SERVER_REVISION, SERVER_VERSION};

    #[tokio::test]
    async fn includes_product_and_build_information() {
        let page = home().await.0;

        assert!(page.contains("Like grep, but for iCal!"));
        assert!(page.contains("https://git.guzek.uk/kguzek/grepcal"));
        for content in ["self_host", "source_repository"] {
            assert!(page.contains(&format!(
                "href=\"{REPOSITORY_URL}?utm_source=grepcal&amp;utm_medium=referral&amp;utm_content={content}\""
            )));
        }
        assert!(page.contains("</main><footer>"));
        assert!(page.contains(
            "Copyright © 2026 <a href=\"https://www.guzek.uk?utm_source=grepcal&amp;utm_medium=referral&amp;utm_content=footer\">Konrad Guzek</a>"
        ));
        assert!(page.contains(&format!(
            "Server revision: <code>v{} ({})</code>",
            SERVER_VERSION, SERVER_REVISION
        )));
    }
}
