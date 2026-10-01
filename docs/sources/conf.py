# Configuration file for the Sphinx documentation builder.

# -- Project information -----------------------------------------------------

import re
from pathlib import Path

project = "zodb-json-codec"
copyright = "2024-2026, BlueDynamics Alliance"  # noqa: A001
author = "Jens Klein and contributors"

# Cargo.toml is the single source of the version (see RELEASE.md).
_cargo = (Path(__file__).parent.parent.parent / "Cargo.toml").read_text()
version = re.search(r'^version = "([^"]+)"', _cargo, re.M).group(1)
release = version

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
            "title": "Ecosystem",
            "url": "https://bluedynamics.github.io/zodb-pgjsonb/ecosystem.html",
            "children": [
                {
                    "title": "Dashboard",
                    "url": "https://bluedynamics.github.io/zodb-pgjsonb/ecosystem.html",
                    "summary": "Overview of all packages",
                },
                {
                    "title": "zodb-pgjsonb",
                    "url": "https://bluedynamics.github.io/zodb-pgjsonb/",
                    "summary": "PostgreSQL JSONB storage",
                },
                {
                    "title": "zodb-json-codec",
                    "url": "https://bluedynamics.github.io/zodb-json-codec/",
                    "summary": "Rust pickle↔JSON transcoder",
                },
                {
                    "title": "plone-pgcatalog",
                    "url": "https://bluedynamics.github.io/plone-pgcatalog/",
                    "summary": "PostgreSQL-backed catalog",
                },
                {
                    "title": "plone-pgthumbor",
                    "url": "https://bluedynamics.github.io/plone-pgthumbor/",
                    "summary": "Thumbor image scaling",
                },
                {
                    "title": "cdk8s-plone",
                    "url": "https://bluedynamics.github.io/cdk8s-plone/",
                    "summary": "Deploy Plone to Kubernetes",
                },
                {
                    "title": "cloud-vinyl",
                    "url": "https://bluedynamics.github.io/cloud-vinyl/",
                    "summary": "Vinyl Cache operator for Kubernetes",
                },
                {
                    "title": "plone.observability",
                    "url": "https://plone.github.io/plone.observability/",
                    "summary": "Health probes, metrics, and tracing",
                },
            ],
        },
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
html_logo = "_static/logo-web.png"
html_favicon = "_static/favicon.ico"
