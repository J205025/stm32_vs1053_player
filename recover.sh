#!/bin/bash
echo "Waiting for device... Keep pressing and releasing the RESET button on the board!"
while true; do
    probe-rs erase --chip STM32F446RE 2>/dev/null
    if [ $? -eq 0 ]; then
        echo "Successfully erased the chip!"
        break
    fi
    sleep 0.1
done
