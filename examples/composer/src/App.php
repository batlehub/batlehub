<?php

declare(strict_types=1);

namespace App;

use Symfony\Component\Console\Application;
use Symfony\Component\Console\Attribute\AsCommand;
use Symfony\Component\Console\Command\Command;
use Symfony\Component\Console\Input\InputInterface;
use Symfony\Component\Console\Output\OutputInterface;

require __DIR__ . '/../vendor/autoload.php';

#[AsCommand(name: 'app:hello', description: 'Prints a greeting')]
class HelloCommand extends Command
{
    protected function execute(InputInterface $input, OutputInterface $output): int
    {
        $output->writeln('<info>Hello from my-app!</info>');
        return Command::SUCCESS;
    }
}

$app = new Application('my-app', '1.0.0');
$app->addCommand(new HelloCommand());
$app->run();
