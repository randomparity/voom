# VOOM syntax highlighting for Vim/Neovim

Provides syntax highlighting for `.voom` policy files.

## Installation

### vim-plug

```vim
Plug 'randomparity/voom', { 'rtp': 'editors/vim' }
```

### lazy.nvim

```lua
{ dir = "path/to/voom/editors/vim" }
```

### Manual

Copy or symlink the `syntax/` and `ftdetect/` directories into your vim
runtime path (`~/.vim/` or `~/.config/nvim/`).
