"""
n8nc: CLI for n8n workflow automation.
"""

try:
    from importlib.metadata import version
    __version__ = version("n8nc")
except ImportError:
    from importlib_metadata import version
    __version__ = version("n8nc")
