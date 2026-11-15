# Sourced by the launcher. Parameters allow testing without real GPUs.
rdp_manager_auto_backend() {
    rdp_sysfs=$1
    rdp_devices=$2
    rdp_nvidia=0
    for rdp_vendor_file in "$rdp_sysfs"/renderD*/device/vendor; do
        [ -r "$rdp_vendor_file" ] || continue
        rdp_node=$(basename "$(dirname "$(dirname "$rdp_vendor_file")")")
        rdp_device="$rdp_devices/$rdp_node"
        [ -r "$rdp_device" ] && [ -w "$rdp_device" ] || continue
        rdp_vendor=$(cat "$rdp_vendor_file")
        case "$rdp_vendor" in
            0x1002|0x8086) printf 'vaapi %s\n' "$rdp_device"; return ;;
            0x10de) rdp_nvidia=1 ;;
        esac
    done
    if [ "$rdp_nvidia" = 1 ]; then printf 'cuda\n'; else printf 'software\n'; fi
}
