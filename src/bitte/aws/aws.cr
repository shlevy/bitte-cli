require "./aws_types.cr"

module AWS
  class Client
    property region : String

    def initialize(@region)
    end

    def aws(cmd, subcmd, args = [] of String)
      aws_args = [
        "--region", region,
        "--output", "json",
        cmd, subcmd ] + args

        output = IO::Memory.new
        Process.run("aws", args: aws_args, output: output, error: STDERR)
        output.to_s
    end

    def auto_scaling_groups
      AWS::Types::Autoscaling::DescribeAutoScalingGroups.from_json(
        aws("autoscaling", "describe-auto-scaling-groups"))
    end

    def describe_instances
      AWS::Types::EC2::DescribeInstances.from_json(
        aws("ec2", "describe-instances"))
    end

    def describe_instances(instance_ids)
      AWS::Types::EC2::DescribeInstances.from_json(
        aws("ec2", "describe-instances", ["--instance-ids"] + instance_ids))
    end

    def list_keys
      AWS::Types::KMS::ListKeys.from_json(
        aws("kms", "list-keys"))
    end

    def reap(instance_id)
      aws("autoscaling", "set-instance-health", [
        "--instance-id", instance_id, "--health-status", "Unhealthy"
      ])
    end
  end
end
