# Using with Jupyter

leani can also be used as a Jupyter kernel.
In a cell, write the same input that you would enter in leani in the terminal.

## Installation

The kernel runs on the Python version of leani. [uv](https://docs.astral.sh/uv/) is required.

```
uv tool install 'leani[jupyter] @ git+https://github.com/watanany/leani'
leani-kernel install
```

The registered "Lean 4 (leani)" kernel can also be used from a JupyterLab that you installed separately.

## Differences from the terminal

- **Abbreviations are replaced only with Tab**. JupyterLab has no mechanism for replacing them with space
- **The first cell takes time**. The first cell starts the engine, so completion of constant names is not available until then
- **In proof mode, each cell holds one tactic**. After running the `:prove` cell, write each tactic in a separate cell
- **The output of `:!` is shown all at once after the command finishes**. A cell cannot send input to the command, so interactive commands such as `vim` cannot be used
