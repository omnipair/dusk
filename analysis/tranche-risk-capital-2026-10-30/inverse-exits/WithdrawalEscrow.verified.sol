pragma solidity ^0.8.24;

import {ReentrancyGuardTransient} from "src/ReentrancyGuardTransient.sol";

interface IERC20 {
    function transfer(address from, uint amount) external returns(bool);
    function approve(address spender, uint amount) external returns(bool);
    function transferFrom(address from, address to, uint amount) external returns(bool);
    function totalSupply() external view returns(uint);
    function balanceOf(address) external view returns(uint);
}

interface IERC4626 is IERC20{
    function redeem(uint shares, address receiver, address owner) external returns(uint);
    function previewRedeem(uint shares) external returns(uint);
    function asset() external view returns(IERC20);
}

interface IDonatableVault is IERC4626 {
    function donate(uint amount) external;
}

interface IWithdrawDelayModel{
    function getWithdrawDelay(uint totalSupply, uint totalWithdrawing, address withdrawer) external returns(uint);
}

contract WithdrawalEscrow is ReentrancyGuardTransient{

    struct ExitWindow{
        uint128 start;
        uint128 end;
    }

    uint public constant MIN_EXIT_WINDOW = 1 days;
    uint public constant MAX_EXIT_WINDOW = 30 days;
    uint public constant MIN_WITHDRAW_DELAY = 1 days;
    uint public constant MAX_WITHDRAW_DELAY = 60 days;
    uint public constant MAX_WITHDRAWAL_PAUSE = 90 days;
    uint public exitWindow = 2 days;
    uint public withdrawFeeBps;
    uint public withdrawalsPausedUntil;
    address public gov;
    address public pendingGov;
    IDonatableVault vault;
    IWithdrawDelayModel public withdrawDelayModel;

    mapping(address => uint) public withdrawAmounts;
    mapping(address => ExitWindow) public exitWindows;

    event Queue(address indexed withdrawer, uint amount, uint fee, uint start, uint end);
    event Withdraw(address indexed withdrawer, uint amount);
    event Cancel(address indexed withdrawer, uint amount, uint start, uint end);
    event SetWithdrawDelayModel(address newModel);
    event SetWithdrawFee(uint newWithdrawFeeBps);
    event SetExitWindow(uint newExitWindow);
    event SetWithdrawalsPausedUntil(uint pausedUntil);
    event NewPendingGov(address);
    event NewGov(address);

    constructor(address _gov, address _withdrawDelayModel){
        gov = _gov;
        withdrawDelayModel = IWithdrawDelayModel(_withdrawDelayModel);
        emit SetWithdrawDelayModel(_withdrawDelayModel);
    }

    modifier isInitialized(){
        require(address(vault) != address(0), "Not initialized");
        _;
    }

    modifier onlyGov(){
        require(msg.sender == gov, "Only gov");
        _;
    }

    modifier withdrawalsNotPaused(){
        require(block.timestamp >= withdrawalsPausedUntil, "Withdrawals paused");
        _;
    }

    function getWithdrawDelay(uint totalSupply, uint totalWithdrawing, address withdrawer) external returns(uint){
        require(msg.sender == address(this), "Only withdrawal escrow");
        return withdrawDelayModel.getWithdrawDelay(totalSupply, totalWithdrawing, withdrawer);
    }
    
    //To renew a withdrawal, queue a 0 amount withdrawal
    function queueWithdrawal(uint amount, uint maxWithdrawDelay) external nonReentrant isInitialized withdrawalsNotPaused {
        uint withdrawDelay;
        try this.getWithdrawDelay(vault.totalSupply(), vault.balanceOf(address(this)) + amount, msg.sender) returns (uint _withdrawDelay){
            if(_withdrawDelay < MIN_WITHDRAW_DELAY){
                withdrawDelay = MIN_WITHDRAW_DELAY;
            } else if(_withdrawDelay > MAX_WITHDRAW_DELAY){
                withdrawDelay = MAX_WITHDRAW_DELAY;
            } else {
                withdrawDelay = _withdrawDelay;
            }
        } catch {
            withdrawDelay = MAX_WITHDRAW_DELAY;
        }
        if(amount > 0)
            vault.transferFrom(msg.sender, address(this), amount);
        uint totalWithdrawAmount = amount + withdrawAmounts[msg.sender];
        require(totalWithdrawAmount > 0, "Zero withdraw amount");
        uint128 exitWindowStart = exitWindows[msg.sender].start;
        uint fee;
        if(withdrawFeeBps > 0){
            //If user has had a chance to withdraw, we apply full fee, otherwise only apply fee on new amount
            fee = totalWithdrawAmount > amount && block.timestamp >= exitWindowStart ?
                totalWithdrawAmount * withdrawFeeBps / 10000 :
                amount * withdrawFeeBps / 10000;
            //Apply minimum of 1 wei fee to avoid fee cicumvention
            if(fee == 0) fee = 1; 
            totalWithdrawAmount -= fee;
        }
        withdrawAmounts[msg.sender] = totalWithdrawAmount;
        uint128 start = uint128(block.timestamp + withdrawDelay);
        //If last exit window is further in the future than new one, we use last exit window
        if(exitWindowStart > start)
            start = exitWindowStart;
        require(start - block.timestamp <= maxWithdrawDelay, "Max withdraw delay exceeded");
        uint128 end = uint128(start + exitWindow);
        exitWindows[msg.sender] = ExitWindow(start, end);
        if(fee > 0 && vault.previewRedeem(fee) > 0){
            uint feeDola = vault.redeem(fee, address(this), address(this));
            vault.asset().approve(address(vault), feeDola);
            vault.donate(feeDola);
        }
        emit Queue(msg.sender, totalWithdrawAmount, fee, start, end); 
    }

    function completeWithdraw() external nonReentrant withdrawalsNotPaused {
        uint withdrawAmount = withdrawAmounts[msg.sender];
        ExitWindow memory _exitWindow = exitWindows[msg.sender];
        require(block.timestamp >= _exitWindow.start, "Exit window hasn't started");
        require(block.timestamp <= _exitWindow.end, "Exit window has ended");
        require(withdrawAmount > 0, "Zero withdraw amount");
        delete exitWindows[msg.sender];
        delete withdrawAmounts[msg.sender];
        vault.redeem(withdrawAmount, msg.sender, address(this));
        emit Withdraw(msg.sender, withdrawAmount);
    }

    function cancelWithdrawal() external nonReentrant {
        uint withdrawAmount = withdrawAmounts[msg.sender];
        require(withdrawAmount > 0, "Zero withdraw amount");
        require(exitWindows[msg.sender].start <= block.timestamp, "Cant cancel before exit window start");
        emit Cancel(msg.sender, withdrawAmount, exitWindows[msg.sender].start, exitWindows[msg.sender].end);
        delete exitWindows[msg.sender];
        delete withdrawAmounts[msg.sender];
        vault.transfer(msg.sender, withdrawAmount);
    }

    function initialize(address _vault) external onlyGov {
        require(address(vault) == address(0), "Already initialized");
        vault = IDonatableVault(_vault);
    }

    function setWithdrawDelayModel(address _withdrawDelayModel) external onlyGov {
        withdrawDelayModel = IWithdrawDelayModel(_withdrawDelayModel);
        emit SetWithdrawDelayModel(_withdrawDelayModel);
    }

    function setWithdrawFee(uint _withdrawFeeBps) external onlyGov {
        require(_withdrawFeeBps <= 100, "Withdraw fee exceed 1%");
        withdrawFeeBps = _withdrawFeeBps;
        emit SetWithdrawFee(_withdrawFeeBps);
    }

    function setExitWindow(uint _exitWindow) external onlyGov {
        require(_exitWindow >= MIN_EXIT_WINDOW, "Exit window below min");
        require(_exitWindow <= MAX_EXIT_WINDOW, "Exit window above max");
        exitWindow = _exitWindow;
        emit SetExitWindow(_exitWindow);
    }
    
    //Set _pausedUntil to before the current block.timestamp to cancel pause
    function setWithdrawalsPausedUntil(uint _pausedUntil) external onlyGov {
        require(_pausedUntil <= block.timestamp + MAX_WITHDRAWAL_PAUSE, "Pause exceeds max");
        withdrawalsPausedUntil = _pausedUntil;
        emit SetWithdrawalsPausedUntil(_pausedUntil);
    }

    function setGov(address _gov) external onlyGov {
        pendingGov = _gov;
        emit NewPendingGov(_gov);
    }

    function acceptGov() external {
        require(msg.sender == pendingGov, "Only pendingGov");
        gov = pendingGov;
        pendingGov = address(0);
        emit NewGov(gov);
    }
}
