pragma solidity ^0.8.24;

contract LinearInterpolationDelayModel {

    uint96 public minDelay;
    uint96 public maxDelay;
    uint16 public maxDelayThresholdBps;
    address public gov;
    address public pendingGov;

    modifier onlyGov() {
        require(msg.sender == gov, "Only gov");
        _;
    }

    event NewMinDelay(uint96);
    event NewMaxDelay(uint96);
    event NewMaxDelayThresholdBps(uint16);
    event NewGov(address);
    event NewPendingGov(address);

    constructor(uint96 _minDelay, uint96 _maxDelay, uint16 _maxDelayThresholdBps, address _gov){
        require(_minDelay <= _maxDelay, "min delay > max delay");
        require(_maxDelayThresholdBps <= 10_000, "Delay threshold cannot exceed 100%");
        minDelay = _minDelay;
        maxDelay = _maxDelay;
        maxDelayThresholdBps = _maxDelayThresholdBps;
        gov = _gov;
    }
    
    //Deliberately not a view function to allow for stateful delay models
    function getWithdrawDelay(uint totalSupply, uint totalWithdrawing, address) external returns(uint){
        uint maxDelayThreshold = totalSupply * maxDelayThresholdBps / 10_000;
        if(totalWithdrawing >= maxDelayThreshold) return maxDelay; //Safety check in case of wonky accounting on the consuming smart contracts end
        return (minDelay * (maxDelayThreshold - totalWithdrawing) + maxDelay * totalWithdrawing) / maxDelayThreshold;
    }

    function setMinDelay(uint96 _minDelay) external onlyGov {
        require(_minDelay <= maxDelay, "min delay > max delay");
        minDelay = _minDelay;
        emit NewMinDelay(_minDelay);
    }

    function setMaxDelay(uint96 _maxDelay) external onlyGov {
        require(_maxDelay >= minDelay, "max delay < min delay");
        maxDelay = _maxDelay;
        emit NewMaxDelay(_maxDelay);
    }

    function setMaxDelayThresholdBps(uint16 _maxDelayThresholdBps) external onlyGov {
        require(_maxDelayThresholdBps <= 10_000, "Delay threshold cannot exceed 100%");
        maxDelayThresholdBps = _maxDelayThresholdBps;
        emit NewMaxDelayThresholdBps(_maxDelayThresholdBps);
    }

    function setPendingGov(address _gov) external onlyGov {
        pendingGov = _gov;
        emit NewPendingGov(_gov);
    }

    function acceptGov() external {
        require(msg.sender == pendingGov, "Only pending gov");
        gov = pendingGov;
        pendingGov = address(0);
        emit NewGov(gov);
    }
}

