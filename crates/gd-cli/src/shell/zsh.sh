__gd_bin() { command gd "$@"; }

# Hook: record every cd to gd history
__gd_hook() {
    __gd_bin hook "$PWD" &>/dev/null &!
}

autoload -Uz add-zsh-hook
add-zsh-hook chpwd __gd_hook

gd() {
    case "$1" in
        link|unlink|config|list|clean|export|init|doctor|setup|update|boost|unboost|version|help|hook|"-h"|"--help"|"-V"|"--version")
            __gd_bin "$@"
            return $?
            ;;
        -)
            builtin cd -
            return $?
            ;;
    esac

    local result
    result="$(__gd_bin "$@")" || return $?
    # 用明確的 if：沒有輸出就單純不跳轉，不要讓 test 的 false 變成函式的失敗狀態
    if [ -n "$result" ]; then
        builtin cd -- "$result"
    fi
}

_gd() {
    local -a subcmds
    subcmds=(
        'link:Link an alias to a path'
        'unlink:Remove a link'
        'config:Get or set preferences'
        'list:List links and stats'
        'clean:Remove invalid entries'
        'export:Export database as JSON'
        'init:Print shell init script'
        'doctor:Check installation health'
    )
    _describe 'gd commands' subcmds
}
compdef _gd gd
