# Configuration file for the Sphinx documentation builder.

# -- Project information -----------------------------------------------------

project = "zodb-json-codec"
copyright = "2024-2026, BlueDynamics Alliance"  # noqa: A001
author = "Jens Klein and contributors"
release = "1.5"

# -- General configuration ---------------------------------------------------

extensions = [
    "myst_parser",
    "sphinxcontrib.mermaid",
    "sphinx_design",
    "sphinx_copybutton",
]

myst_enable_extensions = [
    "deflist",
    "colon_fence",
    "fieldlist",
]

myst_fence_as_directive = ["mermaid"]

templates_path = ["_templates"]
exclude_patterns = []

# mermaid options
mermaid_output_format = "raw"

# -- Options for HTML output -------------------------------------------------

html_theme = "shibuya"

html_theme_options = {
    "logo_target": "/zodb-json-codec/",
    "accent_color": "orange",
    "color_mode": "dark",
    "dark_code": True,
    "nav_links": [
        {
            "title": "GitHub",
            "url": "https://github.com/bluedynamics/zodb-json-codec",
        },
        {
            "title": "PyPI",
            "url": "https://pypi.org/project/zodb-json-codec/",
        },
    ],
}

html_extra_path = ["llms.txt"]
html_static_path = ["_static"]
html_favicon = "_static/favicon.ico"
