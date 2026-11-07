LC_ALL=C
export LC_ALL

platform=$(uname -s 2>/dev/null)
printf 'platform %s\nos %s\nkernel %s\n' "$platform" "$platform" "$(uname -r 2>/dev/null)"

case "$platform" in
    Linux)
        if [ -r /etc/os-release ]; then
            sed -n 's/^PRETTY_NAME=/os /p' /etc/os-release
        fi
        if [ -r /proc/stat ]; then
            awk '/^cpu / {printf "cpu %.0f %.0f\n", $2+$3+$4+$5+$6+$7+$8+$9, $5+$6}
                 /^cpu[0-9]/ {cores++} END {if (cores) print "cores", cores}' /proc/stat
        fi
        if [ -r /proc/meminfo ]; then
            awk '/^MemTotal:/ {t=$2} /^MemAvailable:/ {a=$2} /^MemFree:/ {m=$2}
                 /^Buffers:/ {buffers=$2} /^Cached:/ {cached=$2}
                 /^SwapTotal:/ {s=$2} /^SwapFree:/ {f=$2}
                 END {if (a == "" && m != "") a=m+buffers+cached
                      if (t && a != "") printf "memory %.0f %.0f\n", t*1024, a*1024
                      if (s != "" && f != "") printf "swap %.0f %.0f\n", s*1024, f*1024}' /proc/meminfo
        fi
        if [ -r /proc/loadavg ]; then awk '{print "load", $1, $2, $3}' /proc/loadavg; fi
        if [ -r /proc/uptime ]; then awk '{printf "uptime %.0f\n", int($1)}' /proc/uptime; fi
        if [ -r /proc/net/dev ]; then
            awk 'NR > 2 {split($0, parts, ":"); gsub(/[[:space:]]/, "", parts[1])
                 if (parts[1] == "lo" || parts[1] ~ /^veth/) next
                 sub(/^[^:]+:[[:space:]]*/, "")
                 print "net", parts[1], $1, $9}' /proc/net/dev
        fi
        ps -eo pid=,pcpu=,pmem=,comm= --sort=-pcpu 2>/dev/null |
            awk 'NR <= 10 {print "process", $0}'
        ;;
    Darwin|FreeBSD|OpenBSD|NetBSD|DragonFly)
        if [ "$platform" = Darwin ]; then
            printf 'os macOS %s\n' "$(sw_vers -productVersion 2>/dev/null)"
            vm_stat 2>/dev/null |
                awk -v total="$(sysctl -n hw.memsize 2>/dev/null)" '
                    /page size of/ {for (i=1; i<NF; i++) if ($i == "of") page=$(i+1)}
                    /^(Pages free|Pages inactive|Pages speculative):/ {available += $NF}
                    END {if (total && page) printf "memory %.0f %.0f\n", total, available*page}'
            sysctl -n vm.swapusage 2>/dev/null |
                awk '{for (i=1; i<NF-1; i++) {
                    if ($i == "total") total = $(i+2); if ($i == "free") free = $(i+2)
                }
                if (total != "" && free != "") printf "swap %.0f %.0f\n", bytes(total), bytes(free)}
                function bytes(n) {
                    if (n ~ /G$/) return n * 1073741824; if (n ~ /M$/) return n * 1048576
                    if (n ~ /K$/) return n * 1024; return n
                }'
            top -l 2 -s 1 -n 0 2>/dev/null |
                awk '/^CPU usage:/ {for (i=2; i<NF; i++) if ($(i+1) == "idle") idle=$i}
                     END {if (idle != "") printf "cpu_percent %.2f\n", 100 - idle}'
        else
            sysctl -n kern.cp_time 2>/dev/null |
                awk '{gsub(/,/, " "); if (NF >= 5) printf "cpu %.0f %.0f\n", $1+$2+$3+$4+$5, $5}'
            sysctl -n vm.stats.vm.v_free_count vm.stats.vm.v_inactive_count vm.stats.vm.v_cache_count 2>/dev/null |
                awk -v total="$(sysctl -n hw.physmem 2>/dev/null)" -v page="$(getconf PAGESIZE 2>/dev/null)" '
                    {available += $1} END {
                        if (NR >= 2 && total && page) printf "memory %.0f %.0f\n", total, available*page
                    }'
            swapinfo -k 2>/dev/null |
                awk 'NR > 1 && $1 != "Total" {total += $2; free += $4}
                     END {printf "swap %.0f %.0f\n", total*1024, free*1024}'
        fi
        printf 'cores %s\n' "$(sysctl -n hw.ncpu 2>/dev/null)"
        sysctl -n kern.boottime 2>/dev/null |
            awk -v now="$(date +%s)" '{
                for (i=1; i<NF-1; i++) if ($i == "sec") boot=$(i+2)+0
                if (boot > 0 && now >= boot) printf "uptime %.0f\n", now-boot
            }'
        sysctl -n vm.loadavg 2>/dev/null |
            awk '{gsub(/[{}]/, ""); if (NF >= 3) print "load", $1, $2, $3}'
        netstat -ibn 2>/dev/null |
            awk 'NR == 1 {for (i=1; i<=NF; i++) {
                     if ($i == "Ibytes") rx=i; if ($i == "Obytes") tx=i
                 }; next}
                 rx && tx && $3 ~ /^<Link#/ && $1 !~ /^(lo[0-9]*$|veth)/ {
                     print "net", $1, $rx, $tx
                 }'
        ps -axo pid=,pcpu=,pmem=,comm= 2>/dev/null | sort -rn -k2 |
            awk 'NR <= 10 {print "process", $0}'
        ;;
    *) exit 0 ;;
esac

if [ "$platform" = Linux ]; then
    if disks=$(df -P -B1 -x tmpfs -x devtmpfs -x overlay 2>/dev/null) || [ -n "$disks" ]; then
        block_size=1
    elif disks=$(df -Pk -x tmpfs -x devtmpfs -x overlay 2>/dev/null) || [ -n "$disks" ]; then
        block_size=1024
    elif disks=$(df -PkT 2>/dev/null) || [ -n "$disks" ]; then
        block_size=1024
        disks=$(printf '%s\n' "$disks" | awk '
            $2 != "tmpfs" && $2 != "devtmpfs" && $2 != "overlay" {
                device=$1
                sub(/^[[:space:]]*[^[:space:]]+[[:space:]]+[^[:space:]]+[[:space:]]+/, "")
                print device, $0
            }')
    fi
else
    disks=$(df -Pk 2>/dev/null)
    block_size=1024
fi
printf '%s\n' "$disks" |
    awk -v block_size="$block_size" 'NR > 1 && NF >= 6 {
        printf "disk %s %.0f %.0f ", $5, $3*block_size, $2*block_size
        sub(/^[[:space:]]*/, "")
        for (i=1; i<=5; i++) sub(/^[^[:space:]]+[[:space:]]+/, "")
        print
    }'
exit 0
